use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const SENSITIVE_EXTENSIONS: &[&str] = &[
    "key", "pem", "p12", "pfx", "kdbx", "gpg", "pgp", "asc", "age", "keystore", "jks", "kdb",
    "der", "crt", "cer", "psafe3", "ibak",
];
pub const SENSITIVE_FILENAMES: &[&str] = &[
    "credentials",
    "credentials.json",
    "secrets.json",
    "passwords.txt",
    "id_rsa",
    "id_ed25519",
    "id_dsa",
    "id_ecdsa",
    "authorized_keys",
    "known_hosts",
];
pub const GENERATED_DIRECTORIES: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "vendor",
    "venv",
    "__pycache__",
    "containers",
    "lost+found",
];

#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME must be set");
        let base = |name: &str, fallback: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
                .join("gseek")
        };
        Self {
            data: base("XDG_DATA_HOME", ".local/share"),
            config: base("XDG_CONFIG_HOME", ".config"),
            cache: base("XDG_CACHE_HOME", ".cache"),
        }
    }
    pub fn init(&self) -> Result<()> {
        for p in [
            &self.data,
            &self.config,
            &self.cache,
            &self.data.join("models"),
        ] {
            fs::create_dir_all(p)?;
            fs::set_permissions(p, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    pub fn database(&self) -> PathBuf {
        self.data.join("index.sqlite3")
    }
    pub fn settings(&self) -> PathBuf {
        self.config.join("settings.json")
    }
    pub fn load(&self) -> Result<Settings> {
        if !self.settings().exists() {
            return Ok(Settings::default());
        }
        let result: Settings = serde_json::from_slice(&fs::read(self.settings())?)
            .context("Cannot read settings; the existing file has been preserved")?;
        result.validate()?;
        Ok(result)
    }
    pub fn save(&self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        self.init()?;
        atomic_write(&self.settings(), &serde_json::to_vec_pretty(settings)?)
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing parent directory")?)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Auto,
    Cpu,
    Vulkan,
    Cuda,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Cpu => "CPU",
            Self::Vulkan => "Vulkan",
            Self::Cuda => "NVIDIA CUDA",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub configured: bool,
    pub folders: Vec<PathBuf>,
    pub excluded_folders: Vec<PathBuf>,
    pub excluded_extensions: Vec<String>,
    pub backend: Backend,
    pub external: Option<External>,
    pub paused: bool,
    pub images: bool,
    pub audio_video: bool,
    pub search_enabled: bool,
    pub max_file_mb: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct External {
    pub url: String,
    pub model: String,
    pub allow_remote: bool,
}

impl External {
    pub fn validate(&self) -> Result<url::Url> {
        let url = url::Url::parse(&self.url)?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("Use an endpoint without credentials, query parameters, or a fragment");
        }
        let local = is_loopback(&url);
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            bail!("Use an HTTP or HTTPS endpoint");
        }
        if !local && (!self.allow_remote || url.scheme() != "https") {
            bail!("Remote endpoints require HTTPS and explicit permission to send file content");
        }
        if self.model.trim().is_empty() {
            bail!("Choose an EmbeddingGemma 2 model identifier");
        }
        Ok(url)
    }
}

pub fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}

impl Default for Settings {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        // GLib honours translated XDG user directories. Never fall back to the entire home folder.
        let folders = [
            glib::UserDirectory::Documents,
            glib::UserDirectory::Desktop,
            glib::UserDirectory::Pictures,
            glib::UserDirectory::Music,
            glib::UserDirectory::Videos,
        ]
        .into_iter()
        .filter_map(glib::user_special_dir)
        .filter(|p| p != &home)
        .collect();
        Self {
            version: 1,
            configured: false,
            folders,
            excluded_folders: vec![],
            excluded_extensions: vec![],
            backend: Backend::Auto,
            external: None,
            paused: false,
            images: true,
            audio_video: true,
            search_enabled: false,
            max_file_mb: 2048,
        }
    }
}

impl Settings {
    /// Merge an individual preference against the latest saved settings.
    /// This prevents one open page from overwriting unrelated changes.
    pub fn patched(&self, patch: serde_json::Value) -> Result<Self> {
        let patch = patch.as_object().context("Preferences must be an object")?;
        let mut current = serde_json::to_value(self)?;
        let fields = current.as_object_mut().unwrap();
        for (name, value) in patch {
            if !fields.contains_key(name) || matches!(name.as_str(), "version" | "configured") {
                bail!("Unknown or protected preference: {name}");
            }
            fields.insert(name.clone(), value.clone());
        }
        let settings: Self = serde_json::from_value(current)?;
        settings.validate()?;
        Ok(settings)
    }
    pub fn location_label(&self, path: &Path) -> String {
        self.folders
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .map(|root| {
                let mut label = PathBuf::from(root.file_name().unwrap_or_default());
                if let Ok(relative) = path.strip_prefix(root) {
                    label.push(relative);
                }
                label.to_string_lossy().into_owned()
            })
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("Unsupported settings version {}", self.version);
        }
        for p in self.folders.iter().chain(&self.excluded_folders) {
            if !p.is_absolute() || p == Path::new("/") {
                bail!("Choose an absolute user folder, not the filesystem root");
            }
            if p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                bail!("Folder paths cannot contain ..");
            }
        }
        if !(1..=16384).contains(&self.max_file_mb) {
            bail!("File limit must be between 1 and 16384 MB");
        }

        if let Some(external) = &self.external {
            external.validate()?;
        }
        Ok(())
    }

    pub fn allows(&self, path: &Path) -> bool {
        self.folders.iter().any(|p| path.starts_with(p)) && self.allows_filename(path)
    }

    /// Filename search follows LocalSearch's scope, without reading file contents.
    /// Explicit exclusions and privacy rules apply to both kinds of results.
    pub fn allows_filename(&self, path: &Path) -> bool {
        !self.excluded_folders.iter().any(|p| path.starts_with(p))
            && !path.components().any(|c| {
                let s = c.as_os_str().to_string_lossy();
                s.starts_with('.') || GENERATED_DIRECTORIES.contains(&s.as_ref())
            })
            && !self.excluded_extensions.iter().any(|ext| {
                path.extension().is_some_and(|e| {
                    e.to_string_lossy()
                        .eq_ignore_ascii_case(ext.trim_start_matches('.'))
                })
            })
            && !sensitive(path)
    }
}

fn sensitive(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    SENSITIVE_FILENAMES.contains(&name.as_str())
        || SENSITIVE_EXTENSIONS.contains(
            &path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase()
                .as_str(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_keep_downloads_out_of_content_search() {
        let settings = Settings::default();
        if let Some(downloads) = glib::user_special_dir(glib::UserDirectory::Downloads) {
            assert!(!settings.folders.contains(&downloads));
        }
        let settings = Settings {
            folders: vec!["/home/test/Documents".into()],
            ..settings
        };
        assert!(!settings.allows(Path::new("/home/test/Downloads/invoice.pdf")));
        assert!(settings.allows_filename(Path::new("/home/test/Downloads/invoice.pdf")));
        for path in [
            "/home/test/Downloads/.hidden.txt",
            "/home/test/Downloads/backup.gpg",
            "/home/test/Documents/encryption.age",
            "/home/test/Documents/id_ecdsa",
            "/home/test/Documents/secret.key",
            "/home/test/Downloads/passwords.psafe3",
            "/home/test/Downloads/passwords.IBAK",
        ] {
            assert!(!settings.allows_filename(Path::new(path)), "{path}");
            assert!(!settings.allows(Path::new(path)), "{path}");
        }
    }
    #[test]
    fn preference_patches_preserve_other_changes_and_validate_before_saving() -> Result<()> {
        let initial = Settings::default();
        let cuda = initial.patched(serde_json::json!({"backend":"cuda"}))?;
        let folders =
            cuda.patched(serde_json::json!({"excluded_folders":["/home/test/private"]}))?;
        assert_eq!(folders.backend, Backend::Cuda);
        assert!(!folders.configured);
        assert_eq!(
            folders.excluded_folders,
            vec![PathBuf::from("/home/test/private")]
        );
        assert!(
            initial
                .patched(serde_json::json!({"backend":"unknown"}))
                .is_err()
        );
        assert!(
            initial
                .patched(serde_json::json!({"configured":true}))
                .is_err()
        );
        assert!(
            initial
                .patched(serde_json::json!({"excluded_folders":["/"]}))
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn scope_is_component_aware_and_excludes_private_generated_data() {
        let mut s = Settings {
            folders: vec!["/home/test/Documents".into()],
            ..Default::default()
        };
        assert!(s.allows(Path::new("/home/test/Documents/Arabic/ملاحظات.txt")));
        for p in [
            "/home/test/Documents-old/secret.txt",
            "/home/test/Documents/.ssh/id_rsa",
            "/home/test/Documents/node_modules/a.js",
            "/home/test/Documents/cert.pem",
            "/home/test/Documents/secrets.json",
        ] {
            assert!(!s.allows(Path::new(p)), "{p}");
        }
        s.excluded_folders
            .push("/home/test/Documents/private".into());
        assert!(!s.allows(Path::new("/home/test/Documents/private/x.txt")));
        assert!(s.allows(Path::new("/home/test/Documents/private-notes.txt")));
    }
    #[test]
    fn remote_upload_requires_explicit_consent_and_tls() {
        for endpoint in [
            "http://192.168.1.5:8080/v1",
            "https://example.com/v1",
            "http://127.0.0.1.evil/v1",
        ] {
            assert!(
                External {
                    url: endpoint.into(),
                    model: "embeddinggemma-2".into(),
                    allow_remote: false
                }
                .validate()
                .is_err()
            );
        }
        for endpoint in [
            "http://127.0.0.1:8080/v1",
            "http://[::1]:8080/v1",
            "http://localhost:8080/v1",
        ] {
            assert!(
                External {
                    url: endpoint.into(),
                    model: "embeddinggemma-2".into(),
                    allow_remote: false
                }
                .validate()
                .is_ok()
            );
        }
    }
    #[test]
    fn corrupt_settings_are_preserved() -> Result<()> {
        let d = tempfile::tempdir()?;
        let p = Paths {
            data: d.path().join("data"),
            config: d.path().join("config"),
            cache: d.path().join("cache"),
        };
        p.init()?;
        fs::write(p.settings(), b"broken")?;
        assert!(p.load().is_err());
        assert_eq!(fs::read(p.settings())?, b"broken");
        Ok(())
    }
}
