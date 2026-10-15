use crate::{
    config::{Backend, Paths, Settings},
    extract::Input,
    model,
    store::{DIMENSIONS, validate_vector},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub struct Runtime {
    settings: Settings,
    paths: Paths,
    worker: Option<Worker>,
    query_only: bool,
    cache: VecDeque<(String, Vec<f32>)>,
    pub backend: String,
    last_used: Instant,
}

#[derive(Debug)]
pub struct InferenceError(pub String);
impl std::fmt::Display for InferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InferenceError {}

impl Runtime {
    pub fn new(paths: &Paths, settings: &Settings, query_only: bool) -> Self {
        Self {
            settings: settings.clone(),
            paths: paths.clone(),
            worker: None,
            query_only,
            cache: VecDeque::new(),
            backend: String::new(),
            last_used: Instant::now(),
        }
    }
    pub fn fingerprint(&self) -> String {
        if let Some(e) = &self.settings.external {
            format!(
                "external:{}:{}:embeddinggemma2-768-v1-readers1",
                e.url, e.model
            )
        } else {
            model::PIPELINE.into()
        }
    }
    pub fn validate(&mut self) -> Result<()> {
        let vector = self.query("the clause about pets in the lease")?;
        let reference: Vec<f32> =
            serde_json::from_str(include_str!("../data/reference-embedding.json"))?;
        let cosine: f32 = vector.iter().zip(reference).map(|(a, b)| a * b).sum();
        if cosine < 0.995 {
            bail!(
                "The inference engine's vectors do not match EmbeddingGemma 2 (compatibility cosine {cosine:.4}). Check its model weights, task prompts, mean pooling, and normalization."
            );
        }
        Ok(())
    }
    pub fn query(&mut self, text: &str) -> Result<Vec<f32>> {
        let text = text.chars().take(4096).collect::<String>();
        if let Some((_, v)) = self.cache.iter().find(|(s, _)| s == &text) {
            return Ok(v.clone());
        }
        let vector = self.embed(&Input::Text {
            text: format!("task: search result | query: {text}"),
        })?;
        self.cache.push_front((text, vector.clone()));
        self.cache.truncate(32);
        Ok(vector)
    }
    pub fn release_idle(&mut self) {
        if self.last_used.elapsed() > Duration::from_secs(120) {
            self.worker = None;
        }
    }
    pub fn embed(&mut self, input: &Input) -> Result<Vec<f32>> {
        self.embed_inner(input)
            .map_err(|e| InferenceError(format!("{e:#}")).into())
    }
    fn embed_inner(&mut self, input: &Input) -> Result<Vec<f32>> {
        self.last_used = Instant::now();
        if let Some(e) = &self.settings.external {
            let url = e.validate()?;
            let Input::Text { text } = input else {
                bail!(
                    "This external endpoint exposes text embeddings only. Use managed inference for images, PDF scans, audio, and video."
                );
            };
            // Disable redirects: a server must not forward document content to another machine.
            let client = model::agent(Duration::from_secs(15), 0, crate::config::is_loopback(&url));
            let address = format!("{}/embeddings", url.as_str().trim_end_matches('/'));
            let response: Value = client
                .post(&address)
                .send_json(json!({"model":e.model,"input":text,"encoding_format":"float"}))?
                .body_mut()
                .read_json()?;
            let returned = response["model"]
                .as_str()
                .context("Endpoint did not identify its embedding model")?;
            if returned != e.model {
                bail!(
                    "Endpoint returned a different model ({returned}); expected {}",
                    e.model
                );
            }
            if !e.model.to_ascii_lowercase().contains("embeddinggemma-2")
                && !e.model.to_ascii_lowercase().contains("embeddinggemma2")
            {
                bail!("External model must be an EmbeddingGemma 2 model");
            }
            let mut vector: Vec<f32> =
                serde_json::from_value(response["data"][0]["embedding"].clone())?;
            normalize(&mut vector)?;
            self.backend = "External · text only".into();
            return Ok(vector);
        }
        if self.worker.is_none() {
            let mut failures = vec![];
            for backend in candidates(self.settings.backend) {
                match Worker::start(&self.paths, &self.settings, self.query_only, backend) {
                    Ok(worker) => {
                        self.worker = Some(worker);
                        self.backend = format!("{backend:?}");
                        break;
                    }
                    Err(e) => failures.push(format!("{backend:?}: {e:#}")),
                }
            }
            if self.worker.is_none() {
                bail!("Cannot start EmbeddingGemma 2. {}", failures.join("\n"));
            }
        }
        let result = self.worker.as_mut().unwrap().embed(input);
        if result.is_err() {
            self.worker = None;
            self.cache.clear();
        }
        result
    }
}

pub fn candidates(selected: Backend) -> Vec<Backend> {
    if selected != Backend::Auto {
        return vec![selected];
    }
    let mut result = vec![];
    if PathBuf::from("/dev/nvidiactl").exists() {
        result.push(Backend::Cuda);
    }
    if PathBuf::from("/dev/dri").exists() {
        result.push(Backend::Vulkan);
    }
    result.push(Backend::Cpu);
    result
}

pub fn runtime_directory(paths: &Paths, backend: Backend) -> Result<PathBuf> {
    let name = format!("{backend:?}").to_lowercase();
    if let Some(dir) = std::env::var_os("GSEEK_RUNTIME_DIR") {
        let dir = PathBuf::from(dir);
        if dir.join(&name).is_dir() {
            return Ok(dir.join(name));
        }
        if dir.join("gseek-inference").is_file() {
            return Ok(dir);
        }
    }
    let exe = std::env::current_exe()?;
    let prefix = exe.parent().context("Missing executable directory")?;
    for dir in [
        prefix.join("runtime").join(&name),
        prefix.join("../libexec/gseek/runtime").join(&name),
        prefix.join("../../dist/runtime").join(&name),
    ] {
        if dir.join("gseek-inference").is_file() {
            return Ok(dir);
        }
    }
    install_runtime(paths, &name)
}

#[derive(serde::Deserialize)]
struct RuntimeManifest {
    version: u32,
    assets: std::collections::BTreeMap<String, std::collections::BTreeMap<String, RuntimeAsset>>,
}
#[derive(serde::Deserialize)]
struct RuntimeAsset {
    url: String,
    sha256: String,
    bytes: u64,
}

fn install_runtime(paths: &Paths, backend: &str) -> Result<PathBuf> {
    let manifest = std::env::var_os("GSEEK_RUNTIME_MANIFEST").context(
        "This development build has no managed runtime manifest; use scripts/build-inference.sh",
    )?;
    let manifest: RuntimeManifest = serde_json::from_slice(&fs::read(manifest)?)?;
    if manifest.version != 1 {
        bail!("Unsupported runtime manifest; reinstall GSeek");
    }
    let asset=manifest.assets.get(backend).and_then(|a|a.get(std::env::consts::ARCH))
        .with_context(||format!("The {backend} backend is not distributed for this architecture; choose Automatic or Vulkan"))?;
    let prefix = format!(
        "https://github.com/FarisZR/GSeek/releases/download/v{}/",
        env!("CARGO_PKG_VERSION")
    );
    if !asset.url.starts_with(&prefix)
        || asset.sha256.len() != 64
        || !asset.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("Invalid managed runtime metadata");
    }
    let root = paths.data.join("runtimes");
    fs::create_dir_all(&root)?;
    let dest = root.join(format!("{backend}-{}", &asset.sha256[..16]));
    if dest.join("gseek-inference").is_file() {
        return Ok(dest);
    }
    let archive = root.join(format!("{backend}-{}.tar.gz", &asset.sha256[..16]));
    model::download_verified(&asset.url, &archive, asset.bytes, &asset.sha256, |_, _| {
        Ok(())
    })?;
    let temp = tempfile::tempdir_in(&root)?;
    let status = Command::new(crate::extract::tool("bsdtar"))
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(temp.path())
        .args(["--no-same-owner", "--no-same-permissions"])
        .stdin(Stdio::null())
        .status()?;
    if !status.success() || !temp.path().join(backend).join("gseek-inference").is_file() {
        bail!("Cannot unpack the verified {backend} runtime");
    }
    fs::rename(temp.path().join(backend), &dest)?;
    fs::remove_file(archive)?;
    Ok(dest)
}

struct Worker {
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<Result<Value, String>>,
    next_id: u64,
}
impl Worker {
    fn start(
        paths: &Paths,
        settings: &Settings,
        query_only: bool,
        backend: Backend,
    ) -> Result<Self> {
        let dir = runtime_directory(paths, backend)?;
        let models = paths.data.join("models");
        for file in if query_only {
            &model::FILES[..1]
        } else {
            &model::FILES[..]
        } {
            if fs::metadata(models.join(file.name))
                .map(|m| m.len())
                .unwrap_or(0)
                != file.bytes
            {
                bail!("The model download is incomplete. Open GSeek and choose Retry Setup.");
            }
        }
        let mut cmd = Command::new(dir.join("gseek-inference"));
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl/getppid are async-signal-safe on Linux. A crashed or stopped service
        // must not leave a model process holding GPU memory after its parent disappears.
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() == 1 {
                    libc::raise(libc::SIGKILL);
                }
                Ok(())
            });
        }
        let log_path = paths.data.join("inference.log");
        if fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0) > 2 * 1024 * 1024 {
            let _ = fs::rename(&log_path, paths.data.join("inference.previous.log"));
        }
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        cmd.arg(models.join(model::FILES[0].name))
            .arg(if query_only {
                PathBuf::from("-")
            } else {
                models.join(model::FILES[1].name)
            })
            .arg(&dir)
            .arg(if backend == Backend::Cpu {
                "cpu"
            } else {
                "gpu"
            })
            .arg(
                std::thread::available_parallelism()
                    .map(|n| n.get().min(4))
                    .unwrap_or(2)
                    .to_string(),
            )
            .env(
                "LD_LIBRARY_PATH",
                std::env::join_paths(std::iter::once(dir.clone()).chain(std::env::split_paths(
                    &std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
                )))?,
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log);
        if backend == Backend::Rocm
            && let Some(v) = &settings.rocm_override
        {
            cmd.env("HSA_OVERRIDE_GFX_VERSION", v);
        }
        if backend == Backend::Rocm {
            cmd.env("ROCBLAS_TENSILE_LIBPATH", dir.join("rocblas/library"));
        }
        if backend == Backend::Vulkan
            && !std::path::Path::new("/dev/dri").exists()
            && std::env::var_os("GGML_VK_VISIBLE_DEVICES").is_none()
        {
            cmd.env("GGML_VK_VISIBLE_DEVICES", "0");
        }
        let mut child = cmd
            .spawn()
            .context("Cannot launch the managed embedding worker")?;
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, output) = mpsc::sync_channel(2);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let result = reader.by_ref().take(256 * 1024).read_line(&mut line);
                let value = match result {
                    Ok(0) => break,
                    Ok(_) => serde_json::from_str(&line).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        let worker = Self {
            child,
            input,
            output,
            next_id: 0,
        };
        let hello = worker.receive(Duration::from_secs(90)).with_context(|| {
            let log = fs::read_to_string(paths.data.join("inference.log")).unwrap_or_default();
            let tail = log
                .lines()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            format!("Managed runtime did not become ready. {tail}")
        })?;
        if hello["ready"] != true
            || hello["model"] != "google/embeddinggemma-2"
            || hello["dimensions"] != DIMENSIONS
            || hello["protocol"] != 1
        {
            bail!("Managed runtime has an incompatible model or protocol");
        }
        Ok(worker)
    }
    fn receive(&self, timeout: Duration) -> Result<Value> {
        self.output
            .recv_timeout(timeout)
            .context(
                "Embedding worker stopped or timed out; see Indexing & Activity for diagnostics",
            )?
            .map_err(anyhow::Error::msg)
    }
    fn embed(&mut self, input: &Input) -> Result<Vec<f32>> {
        self.next_id += 1;
        serde_json::to_writer(&mut self.input, &json!({"id":self.next_id,"input":input}))?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        let timeout = std::env::var("GSEEK_INFERENCE_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(1, 600);
        let response = self.receive(Duration::from_secs(timeout))?;
        if let Some(error) = response["error"].as_str() {
            bail!("Embedding failed: {error}");
        }
        if response["id"] != self.next_id {
            bail!("Embedding response does not match the pending request");
        }
        let vector: Vec<f32> = serde_json::from_value(response["vector"].clone())?;
        validate_vector(&vector)?;
        Ok(vector)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn normalize(vector: &mut [f32]) -> Result<()> {
    if vector.len() != DIMENSIONS || vector.iter().any(|v| !v.is_finite()) {
        bail!("Endpoint must return {DIMENSIONS} finite dimensions");
    }
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-10 {
        bail!("Endpoint returned a zero vector");
    }
    for v in vector.iter_mut() {
        *v /= norm;
    }
    validate_vector(vector)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_device_does_not_silently_select_something_else() {
        assert_eq!(candidates(Backend::Cuda), vec![Backend::Cuda]);
        assert_eq!(candidates(Backend::Rocm), vec![Backend::Rocm]);
    }
    #[test]
    fn external_vectors_must_have_the_right_contract() {
        assert!(normalize(&mut [0.0; 768]).is_err());
        assert!(normalize(&mut [f32::NAN; 768]).is_err());
        assert!(normalize(&mut [1.0; 12]).is_err());
        let mut v = vec![2.0; 768];
        normalize(&mut v).unwrap();
        validate_vector(&v).unwrap();
    }
}
