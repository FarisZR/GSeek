use crate::{
    cancellation::Cancellation,
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
    io::{Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
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
    cancellation: Cancellation,
    progress: Progress,
}

type Progress = Arc<dyn Fn(&str, u64, u64) + Send + Sync>;

#[derive(Debug)]
pub struct InferenceError(pub String);
impl std::fmt::Display for InferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InferenceError {}

/// An unsuitable file must not restart a healthy engine or stop the whole queue.
#[derive(Debug)]
struct InputError(String);
impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InputError {}

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
            cancellation: Cancellation::default(),
            progress: Arc::new(|_, _, _| {}),
        }
    }
    pub(crate) fn with_cancellation(mut self, cancellation: Cancellation) -> Self {
        self.cancellation = cancellation;
        self
    }
    pub(crate) fn with_progress(
        mut self,
        progress: impl Fn(&str, u64, u64) + Send + Sync + 'static,
    ) -> Self {
        self.progress = Arc::new(progress);
        self
    }
    pub(crate) fn cancelled(&self) -> bool {
        self.cancellation.check().is_err()
    }
    pub fn fingerprint(&self) -> String {
        if let Some(e) = &self.settings.external {
            format!(
                "external:{}:{}:embeddinggemma2-768-v1-{}",
                e.url,
                e.model,
                crate::extract::READER_VERSION
            )
        } else {
            format!("{}-{}", model::PIPELINE, crate::extract::READER_VERSION)
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
        let result = self.embed_inner(input);
        self.cancellation.check()?;
        result.map_err(|e| {
            if e.is::<InputError>() {
                e
            } else {
                InferenceError(format!("{e:#}")).into()
            }
        })
    }
    fn embed_inner(&mut self, input: &Input) -> Result<Vec<f32>> {
        self.cancellation.check()?;
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
                let attempt = Worker::start(
                    &self.paths,
                    self.query_only,
                    backend,
                    &self.cancellation,
                    &self.progress,
                );
                match attempt {
                    Ok(mut worker) => {
                        let result = worker.embed(input);
                        if result.is_ok() || result.as_ref().is_err_and(|e| e.is::<InputError>()) {
                            self.backend = worker.backend.clone();
                            self.worker = Some(worker);
                            return result;
                        }
                        self.cancellation.check()?;
                        failures.push(format!("{}: {:#}", backend.label(), result.unwrap_err()));
                    }
                    Err(e) => {
                        self.cancellation.check()?;
                        failures.push(format!("{}: {e:#}", backend.label()));
                    }
                }
            }
            bail!("Cannot start EmbeddingGemma 2. {}", failures.join("\n"));
        }
        let result = self.worker.as_mut().unwrap().embed(input);
        if result.as_ref().is_err_and(|e| !e.is::<InputError>()) {
            self.worker = None;
            self.cache.clear();
            if self.settings.backend == Backend::Auto {
                // Try fresh workers, including CPU, if a growing GPU context
                // no longer fits or its driver fails. Explicit choices report errors.
                return self.embed_inner(input);
            }
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

// The worker is an unmodified upstream server, reached only through a private socket.
struct Worker {
    child: Arc<Mutex<Child>>,
    client: ureq::Agent,
    media_marker: String,
    token_budget: usize,
    backend: String,
    _socket_dir: tempfile::TempDir,
}
impl Worker {
    fn start(
        paths: &Paths,
        query_only: bool,
        backend: Backend,
        cancellation: &Cancellation,
        progress: &Progress,
    ) -> Result<Self> {
        cancellation.check()?;
        let dir = crate::managed_runtime::directory_with_progress(
            paths,
            backend,
            |detail, done, total| {
                cancellation.check()?;
                progress(detail, done, total);
                Ok(())
            },
        )?;
        progress(&format!("Starting {}", backend.label()), 0, 0);
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
        // This short owner-only path fits Linux's Unix socket path limit even
        // when the user's XDG directory is deeply nested. No TCP port is opened.
        let socket_dir = tempfile::Builder::new()
            .prefix("gseek-")
            .tempdir_in("/tmp")?;
        let socket = socket_dir.path().join("inference.sock");
        let context = if query_only { "512" } else { "2048" };
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(4))
            .unwrap_or(2)
            .to_string();
        let mut cmd = Command::new(dir.join("llama-server"));
        cmd.arg("--model")
            .arg(models.join(model::FILES[0].name))
            .args([
                "--embedding",
                "--log-verbosity",
                "4",
                "--pooling",
                "mean",
                "--ctx-size",
                context,
                "--batch-size",
                context,
                "--ubatch-size",
                context,
                "--parallel",
                "1",
                "--threads",
                &threads,
                "--threads-batch",
                &threads,
                "--gpu-layers",
                // Let upstream fit offloaded layers to available VRAM. An explicit
                // device still requires that GPU backend; it never silently changes it.
                if backend == Backend::Cpu { "0" } else { "auto" },
                "--device",
                match backend {
                    Backend::Cpu => "none",
                    Backend::Vulkan => "Vulkan0",
                    Backend::Cuda => "CUDA0",
                    Backend::Auto => unreachable!(),
                },
                "--flash-attn",
                "off",
                "--no-warmup",
                "--image-min-tokens",
                "1",
                "--image-max-tokens",
                "1120",
                "--no-ui",
                "--host",
            ])
            .arg(&socket);
        if !query_only {
            cmd.arg("--mmproj").arg(models.join(model::FILES[1].name));
        }
        use std::os::unix::process::CommandExt;
        // SAFETY: these Linux calls are async-signal-safe. A service crash must
        // not leave a model process holding GPU memory after its parent exits.
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
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)?;
        let mut startup_log = log.try_clone()?;
        let log_start = log.metadata()?.len();
        cmd.env(
            "LD_LIBRARY_PATH",
            std::env::join_paths(
                std::iter::once(dir.clone())
                    .chain(std::env::split_paths(
                        &std::env::var_os("GSEEK_RUNTIME_LIBRARY_PATH").unwrap_or_default(),
                    ))
                    .chain(std::env::split_paths(
                        &std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
                    )),
            )?,
        )
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
        if backend == Backend::Vulkan
            && !std::path::Path::new("/dev/dri").exists()
            && std::env::var_os("GGML_VK_VISIBLE_DEVICES").is_none()
        {
            cmd.env("GGML_VK_VISIBLE_DEVICES", "0");
        }
        let timeout = std::env::var("GSEEK_INFERENCE_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60)
            .clamp(1, 600);
        let mut worker = Self {
            child: Arc::new(Mutex::new(
                cmd.spawn()
                    .context("Cannot launch the upstream embedding runtime")?,
            )),
            client: crate::local_http::agent(socket.clone(), Duration::from_secs(timeout)),
            media_marker: String::new(),
            token_budget: context.parse().unwrap(),
            backend: backend.label().into(),
            _socket_dir: socket_dir,
        };
        cancellation.track(&worker.child)?;
        let readiness = crate::local_http::agent(socket, Duration::from_secs(1));
        let started = Instant::now();
        loop {
            cancellation.check()?;
            if worker.child.lock().unwrap().try_wait()?.is_some()
                || started.elapsed() > Duration::from_secs(90)
            {
                let log = fs::read_to_string(log_path).unwrap_or_default();
                let tail = log
                    .lines()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                bail!(
                    "{} could not start. Your selected backend has been kept. Check the GPU driver and available memory. Runtime details: {tail}",
                    backend.label()
                );
            }
            if readiness.get("http://127.0.0.1/health").call().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let props: Value = worker
            .client
            .get("http://127.0.0.1/props")
            .call()?
            .body_mut()
            .read_json()?;
        worker.media_marker = props["media_marker"]
            .as_str()
            .context("Upstream runtime has no multimodal API")?
            .to_owned();
        startup_log.seek(SeekFrom::Start(log_start))?;
        let mut details = String::new();
        startup_log.take(1024 * 1024).read_to_string(&mut details)?;
        worker.backend = device_label(backend, &details);
        Ok(worker)
    }
    fn embed(&mut self, input: &Input) -> Result<Vec<f32>> {
        use base64::Engine;
        let content = match input {
            Input::Text { text } => return self.embed_text(text),
            Input::Image { path } | Input::Audio { path } => {
                let file = fs::File::open(path)
                    .map_err(|e| InputError(format!("Cannot read prepared media: {e}")))?;
                if file
                    .metadata()
                    .map_err(|e| InputError(format!("Cannot inspect prepared media: {e}")))?
                    .len()
                    > 32 * 1024 * 1024
                {
                    return Err(InputError(
                        "Prepared media exceeds the embedding input limit".into(),
                    )
                    .into());
                }
                let mut data = vec![];
                file.take(32 * 1024 * 1024 + 1)
                    .read_to_end(&mut data)
                    .map_err(|e| InputError(format!("Cannot read prepared media: {e}")))?;
                if data.len() > 32 * 1024 * 1024 {
                    return Err(InputError(
                        "Prepared media exceeds the embedding input limit".into(),
                    )
                    .into());
                }
                json!({"prompt_string":self.media_marker,"multimodal_data":[base64::engine::general_purpose::STANDARD.encode(data)]})
            }
        };
        self.embed_content(content)
    }

    fn tokenize(&self, text: &str) -> Result<Vec<i32>> {
        let response: Value = self
            .client
            .post("http://127.0.0.1/tokenize")
            .send_json(json!({"content":text,"add_special":true,"parse_special":false}))?
            .body_mut()
            .read_json()?;
        Ok(serde_json::from_value(response["tokens"].clone())?)
    }

    fn text_segments(&self, prefix: &str, text: &str, result: &mut Vec<Vec<i32>>) -> Result<()> {
        let tokens = self.tokenize(&format!("{prefix}{text}"))?;
        if tokens.len() <= self.token_budget {
            result.push(tokens);
            return Ok(());
        }
        // Split only at UTF-8 boundaries. Retokenize each half with its task prompt:
        // character counts alone cannot bound Arabic, CJK, emoji, or source code.
        let midpoint = text
            .char_indices()
            .nth(text.chars().count() / 2)
            .map(|(i, _)| i)
            .filter(|i| *i > 0)
            .ok_or_else(|| InputError("The text cannot fit the model's input limit".into()))?;
        self.text_segments(prefix, &text[..midpoint], result)?;
        self.text_segments(prefix, &text[midpoint..], result)
    }

    fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let (prefix, body) = ["title: none | text: ", "task: search result | query: "]
            .into_iter()
            .find_map(|p| text.strip_prefix(p).map(|s| (p, s)))
            .unwrap_or(("", text));
        let mut segments = vec![];
        self.text_segments(prefix, body, &mut segments)?;
        if segments.len() == 1 {
            return self.embed_content(json!(segments[0]));
        }
        // Preserve every part of a dense text chunk without increasing GPU memory
        // or changing its checkpoint identity. Pool bounded vectors by token count.
        let mut pooled = vec![0.0; DIMENSIONS];
        for tokens in segments {
            let weight = tokens.len() as f32;
            for (sum, value) in pooled.iter_mut().zip(self.embed_content(json!(tokens))?) {
                *sum += value * weight;
            }
        }
        normalize(&mut pooled)?;
        Ok(pooled)
    }

    fn embed_content(&self, content: Value) -> Result<Vec<f32>> {
        let mut reply = self
            .client
            .post("http://127.0.0.1/embedding")
            .config()
            .http_status_as_error(false)
            .build()
            .send_json(json!({"content":content,"embd_normalize":2}))
            .context("Embedding failed; see Indexing & Activity for diagnostics")?;
        let status = reply.status();
        let response: Value = reply.body_mut().read_json()?;
        if !status.is_success() {
            let message = response["error"]["message"]
                .as_str()
                .unwrap_or("Unknown runtime error");
            if status.as_u16() == 400
                || status.as_u16() == 413
                || message.contains("too large to process")
                || message.contains("exceeds the available context")
            {
                return Err(
                    InputError(format!("This file could not be embedded: {message}")).into(),
                );
            }
            bail!("Embedding failed ({status}): {message}");
        }
        let embedding = &response[0]["embedding"];
        let embedding = if embedding[0].is_array() {
            &embedding[0]
        } else {
            embedding
        };
        let vector: Vec<f32> = serde_json::from_value(embedding.clone())?;
        validate_vector(&vector)?;
        Ok(vector)
    }
}

fn device_label(backend: Backend, log: &str) -> String {
    if backend == Backend::Cpu {
        return "CPU".into();
    }
    let device = if backend == Backend::Vulkan {
        "Vulkan0"
    } else {
        "CUDA0"
    };
    let marker = format!("using device {device} (");
    let name = log
        .lines()
        .filter_map(|line| line.split_once(&marker))
        .filter_map(|(_, rest)| rest.split_once(") ("))
        .map(|(name, _)| name)
        .next();
    let Some(name) = name else {
        return format!("{} · device not reported", backend.label());
    };
    let lower = name.to_ascii_lowercase();
    let software = ["llvmpipe", "lavapipe", "swiftshader", "software"]
        .iter()
        .any(|s| lower.contains(s));
    let kind = if software {
        "Software CPU"
    } else if log.contains("offloaded 0/") {
        "CPU · no GPU layers"
    } else {
        "GPU"
    };
    format!("{} · {kind} · {name}", backend.label())
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.lock().unwrap().kill();
        let _ = self.child.lock().unwrap().wait();
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
    fn backend_status_identifies_actual_hardware_and_software_devices() {
        assert_eq!(
            device_label(
                Backend::Vulkan,
                "load: using device Vulkan0 (llvmpipe (LLVM 19, 256 bits)) (unknown id) - 999 MiB free"
            ),
            "Vulkan · Software CPU · llvmpipe (LLVM 19, 256 bits)"
        );
        assert_eq!(
            device_label(
                Backend::Vulkan,
                "load: using device Vulkan0 (AMD Radeon 780M) (id) - 999 MiB free\noffloaded 25/25 layers to GPU"
            ),
            "Vulkan · GPU · AMD Radeon 780M"
        );
        assert_eq!(
            device_label(
                Backend::Cuda,
                "load: using device CUDA0 (NVIDIA RTX A500) (id) - 999 MiB free\noffloaded 0/25 layers to GPU"
            ),
            "NVIDIA CUDA · CPU · no GPU layers · NVIDIA RTX A500"
        );
    }
    #[test]
    #[ignore = "Terminates and restarts a real inference helper; needs the pinned model and runtime"]
    fn worker_restarts_after_process_failure() -> Result<()> {
        let paths = Paths::from_env();
        paths.init()?;
        model::ensure(&paths, |_, _, _| Ok(()))?;
        let mut settings = paths.load()?;
        settings.external = None;
        if settings.backend == Backend::Auto {
            settings.backend = Backend::Cpu;
        }
        let mut runtime = Runtime::new(&paths, &settings, true);
        let input = Input::Text {
            text: "task: search result | query: the clause about pets in the lease".into(),
        };
        let expected = runtime.embed(&input)?;
        let pid = runtime.worker.as_ref().unwrap().child.lock().unwrap().id();
        let error = runtime
            .worker
            .as_ref()
            .unwrap()
            .embed_content(json!(vec![1; 513]))
            .unwrap_err();
        assert!(error.is::<InputError>(), "{error:#}");
        let missing_media = Input::Image {
            path: paths.cache.join("missing-test-image.png"),
        };
        let error = runtime.embed(&missing_media).unwrap_err();
        assert!(error.is::<InputError>());
        let large = paths.cache.join("oversized-test-image.png");
        fs::File::create(&large)?.set_len(32 * 1024 * 1024 + 1)?;
        let error = runtime
            .embed(&Input::Image {
                path: large.clone(),
            })
            .unwrap_err();
        fs::remove_file(large)?;
        assert!(error.is::<InputError>());
        assert_eq!(
            runtime.worker.as_ref().unwrap().child.lock().unwrap().id(),
            pid
        );
        assert_eq!(runtime.embed(&input)?, expected);
        let worker = runtime.worker.as_mut().unwrap();
        worker.child.lock().unwrap().kill()?;
        worker.child.lock().unwrap().wait()?;
        assert!(runtime.embed(&input).is_err());
        assert!(runtime.worker.is_none());
        let actual = runtime.embed(&input)?;
        validate_vector(&actual)?;
        assert!(expected.iter().zip(actual).map(|(a, b)| a * b).sum::<f32>() > 0.999);
        if runtime.settings.backend == Backend::Cpu {
            // CPU jobs also exercise Automatic's transparent retry. Vulkan CI
            // deliberately ships only its own backend, so it checks explicit errors above.
            runtime.settings.backend = Backend::Auto;
            let worker = runtime.worker.as_mut().unwrap();
            worker.child.lock().unwrap().kill()?;
            worker.child.lock().unwrap().wait()?;
            let recovered = runtime.embed(&input)?;
            validate_vector(&recovered)?;
            assert!(
                expected
                    .iter()
                    .zip(recovered)
                    .map(|(a, b)| a * b)
                    .sum::<f32>()
                    > 0.999
            );
        }
        Ok(())
    }
    #[test]
    fn explicit_device_does_not_silently_select_something_else() {
        assert_eq!(candidates(Backend::Cuda), vec![Backend::Cuda]);
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
