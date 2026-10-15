use crate::{
    APP_ID, BUS_NAME,
    config::{Paths, atomic_write},
};
use anyhow::{Context, Result, bail};
use gio::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const PROVIDER: &str = "io.github.FarisZR.GSeek.desktop";
const FILES_PROVIDER: &str = "org.gnome.Nautilus.desktop";
#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    files: BTreeMap<PathBuf, String>,
    search: Option<SearchPrevious>,
}
#[derive(Serialize, Deserialize)]
struct SearchPrevious {
    files_disabled: bool,
    gseek_disabled: bool,
    gseek_enabled: bool,
}

pub fn desktop_quote(path: &Path) -> Result<String> {
    let p = path.to_str().context("Launcher path must be UTF-8")?;
    if p.contains(['\n', '\r']) {
        bail!("Launcher path contains a newline");
    }
    // Desktop Entry Exec is not a shell command. Escape its reserved characters and percent field codes.
    Ok(format!(
        "\"{}\"",
        p.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$")
            .replace('%', "%%")
    ))
}

fn xdg(name: &str, fallback: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(fallback))
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn manifest_path(paths: &Paths) -> PathBuf {
    paths.config.join("integration.json")
}
fn load(paths: &Paths) -> Result<Manifest> {
    let path = manifest_path(paths);
    if !path.exists() {
        return Ok(Manifest::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn save(paths: &Paths, manifest: &Manifest) -> Result<()> {
    atomic_write(&manifest_path(paths), &serde_json::to_vec_pretty(manifest)?)
}

/// Per-user XDG registration keeps Homebrew's prefix out of GNOME's discovery requirements.
/// Absolute, stable launcher paths survive upgrades, login sessions, and a different shell PATH.
pub fn install(paths: &Paths, launcher: &Path) -> Result<()> {
    paths.init()?;
    if !launcher.is_absolute() || !launcher.is_file() {
        bail!("Integration needs an absolute, installed launcher path");
    }
    let data = xdg("XDG_DATA_HOME", ".local/share");
    let config = xdg("XDG_CONFIG_HOME", ".config");
    let exec = desktop_quote(launcher)?;
    let mut manifest = load(paths)?;
    let registrations = [
        (
            data.join(format!("applications/{APP_ID}.desktop")),
            format!(
                "[Desktop Entry]\nType=Application\nName=GSeek\nComment=Find your files by what you remember\nExec={exec}\nIcon={APP_ID}\nTerminal=false\nCategories=GNOME;GTK;Utility;\nKeywords=Search;Files;Semantic;\nStartupNotify=true\n"
            ),
        ),
        (
            data.join(format!("dbus-1/services/{BUS_NAME}.service")),
            format!("[D-BUS Service]\nName={BUS_NAME}\nExec={exec} service\n"),
        ),
        (
            data.join(format!("gnome-shell/search-providers/{APP_ID}.ini")),
            format!(
                "[Shell Search Provider]\nDesktopId={PROVIDER}\nBusName={BUS_NAME}\nObjectPath=/io/github/FarisZR/GSeek/SearchProvider\nVersion=2\nDefaultDisabled=true\n"
            ),
        ),
        (
            config.join(format!("autostart/{APP_ID}.desktop")),
            format!(
                "[Desktop Entry]\nType=Application\nName=GSeek\nExec={exec} service\nIcon={APP_ID}\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\nOnlyShowIn=GNOME;\n"
            ),
        ),
        (
            data.join(format!("icons/hicolor/scalable/apps/{APP_ID}.svg")),
            include_str!("../data/io.github.FarisZR.GSeek.svg").into(),
        ),
    ];
    // Refuse to overwrite another installation or a user's edits. Validate all entries before writing.
    for (path, content) in &registrations {
        if path.exists() {
            let existing = digest(&fs::read(path)?);
            if existing != digest(content.as_bytes()) && manifest.files.get(path) != Some(&existing)
            {
                bail!(
                    "{} is not owned by this GSeek installation; it was preserved",
                    path.display()
                );
            }
        }
    }
    for (path, content) in registrations {
        fs::create_dir_all(path.parent().unwrap())?;
        atomic_write(&path, content.as_bytes())?;
        manifest.files.insert(path, digest(content.as_bytes()));
        save(paths, &manifest)?;
    }
    let _ = std::process::Command::new("update-desktop-database")
        .arg(data.join("applications"))
        .status();
    Ok(())
}

fn search_settings() -> Result<gio::Settings> {
    let source = gio::SettingsSchemaSource::default().context("GNOME settings are unavailable")?;
    let schema = source
        .lookup("org.gnome.desktop.search-providers", true)
        .context("GNOME search settings are unavailable")?;
    Ok(gio::Settings::new_full(
        &schema,
        None::<&gio::SettingsBackend>,
        None,
    ))
}

pub fn enable_search(paths: &Paths) -> Result<()> {
    let settings = search_settings()?;
    let mut manifest = load(paths)?;
    let mut disabled: Vec<String> = settings
        .strv("disabled")
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut enabled: Vec<String> = settings
        .strv("enabled")
        .iter()
        .map(|s| s.to_string())
        .collect();
    if manifest.search.is_none() {
        manifest.search = Some(SearchPrevious {
            files_disabled: disabled.iter().any(|s| s == FILES_PROVIDER),
            gseek_disabled: disabled.iter().any(|s| s == PROVIDER),
            gseek_enabled: enabled.iter().any(|s| s == PROVIDER),
        });
        save(paths, &manifest)?;
    }
    disabled.retain(|s| s != PROVIDER);
    if !disabled.iter().any(|s| s == FILES_PROVIDER) {
        disabled.push(FILES_PROVIDER.into());
    }
    if !enabled.iter().any(|s| s == PROVIDER) {
        enabled.push(PROVIDER.into());
    }
    settings.delay();
    settings.set_strv("disabled", disabled)?;
    settings.set_strv("enabled", enabled)?;
    settings.apply();
    gio::Settings::sync();
    Ok(())
}

pub fn uninstall(paths: &Paths) -> Result<()> {
    let manifest = load(paths)?;
    if let Some(previous) = manifest.search {
        let settings = search_settings()?;
        let mut disabled: Vec<String> = settings
            .strv("disabled")
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut enabled: Vec<String> = settings
            .strv("enabled")
            .iter()
            .map(|s| s.to_string())
            .collect();
        if !previous.files_disabled {
            disabled.retain(|s| s != FILES_PROVIDER);
        }
        disabled.retain(|s| s != PROVIDER);
        if previous.gseek_disabled {
            disabled.push(PROVIDER.into());
        }
        if !previous.gseek_enabled {
            enabled.retain(|s| s != PROVIDER);
        }
        settings.delay();
        settings.set_strv("disabled", disabled)?;
        settings.set_strv("enabled", enabled)?;
        settings.apply();
        gio::Settings::sync();
    }
    for (path, hash) in manifest.files {
        if path.exists() && digest(&fs::read(&path)?) == hash {
            fs::remove_file(path)?;
        }
    }
    if manifest_path(paths).exists() {
        fs::remove_file(manifest_path(paths))?;
    }
    Ok(())
}

pub fn icon_for(path: &Path, mime: &str) -> gio::Icon {
    let file = gio::File::for_path(path);
    if let Ok(info) = file.query_info(
        "standard::icon,thumbnail::path,thumbnail::is-valid",
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    ) {
        if info.boolean("thumbnail::is-valid") {
            if let Some(thumb) = info.attribute_byte_string("thumbnail::path") {
                let thumb = Path::new(thumb.as_str());
                if thumb.is_file() {
                    return gio::FileIcon::new(&gio::File::for_path(thumb)).upcast();
                }
            }
        }
        if let Some(icon) = info.icon() {
            return icon;
        }
    }
    gio::content_type_get_icon(mime)
}

pub fn open_result(paths: &Paths, uri: &str) -> Result<()> {
    let settings = paths.load()?;
    let store = crate::store::Store::open(&paths.database())?;
    let file = store
        .file(uri)?
        .context("This result is no longer indexed")?;
    if crate::discovery::record_from_uri(uri, &settings)?.is_none() || !file.path.exists() {
        bail!("This result moved or is outside your search locations");
    }
    gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_is_escaped_for_desktop_entry_without_a_shell() {
        assert_eq!(
            desktop_quote(Path::new("/tmp/a b/100%/gseek")).unwrap(),
            "\"/tmp/a b/100%%/gseek\""
        );
        assert!(desktop_quote(Path::new("/tmp/bad\nExec=oops")).is_err());
    }
}
