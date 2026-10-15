use crate::{
    BUS_NAME, CONTROL_IFACE, CONTROL_PATH, OBJECT_PATH,
    config::{Paths, Settings},
    discovery, extract,
    inference::Runtime,
    integration, model,
    store::{Activity, Counts, SearchHit, Store, StoredChunk},
};
use anyhow::{Context, Result, bail};
use gio::prelude::*;
use glib::variant::ToVariant;
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const SEARCH_XML: &str = include_str!("../data/search-provider.xml");
const CONTROL_XML: &str = include_str!("../data/service.xml");

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub detail: String,
    pub current: String,
    pub backend: String,
    pub progress: f64,
    pub ready: bool,
    pub paused: bool,
    pub counts: Counts,
    pub last_scan: i64,
}
type Shared = Arc<Mutex<Status>>;
#[derive(Debug)]
enum Command {
    Setup,
    Scan,
    Retry,
    Reindex(Option<PathBuf>),
    Wake,
    Quit,
}

fn status(shared: &Shared, state: &str, detail: impl Into<String>) {
    let mut s = shared.lock().unwrap();
    s.state = state.into();
    s.detail = detail.into();
}

pub fn run(paths: Paths) -> Result<()> {
    paths.init()?;
    let _ = paths.load()?;
    Store::open(&paths.database())?;
    let main_loop = glib::MainLoop::new(None, false);
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    let shared = Arc::new(Mutex::new(Status {
        state: "starting".into(),
        detail: "Starting GSeek".into(),
        ..Default::default()
    }));
    let (tx, rx) = mpsc::channel();
    let worker_paths = paths.clone();
    let worker_state = shared.clone();
    let search = Rc::new(SearchQueue::new(&paths));
    let subscriptions = Rc::new(std::cell::RefCell::new(Vec::new()));
    let live_subscriptions = subscriptions.clone();
    let startup_paths = paths.clone();
    let startup_shared = shared.clone();
    let startup_tx = tx.clone();
    {
        // Export interfaces before owning the name, so the very first activating call is safe.
        let info = gio::DBusNodeInfo::for_xml(SEARCH_XML).unwrap().interfaces()[0].clone();
        let spaths = startup_paths.clone();
        let queue = search.clone();
        let state = startup_shared.clone();
        connection
            .register_object(OBJECT_PATH, &info)
            .method_call(
                move |_connection, _sender, _path, _iface, method, parameters, invocation| {
                    let errors = invocation.clone();
                    let result = (|| -> Result<()> {
                        match method {
                            "GetInitialResultSet" | "GetSubsearchResultSet" => {
                                let index = if method == "GetInitialResultSet" {
                                    0
                                } else {
                                    1
                                };
                                let terms = parameters
                                    .child_value(index)
                                    .get::<Vec<String>>()
                                    .context("Invalid search terms")?;
                                let query = terms.join(" ");
                                if !state.lock().unwrap().ready {
                                    bail!("{}", state.lock().unwrap().detail);
                                }
                                let response = queue.submit(query, 20, true)?;
                                glib::spawn_future_local(async move {
                                    match response.recv().await {
                                        Ok(Ok(hits)) => invocation.return_value(Some(
                                            &(hits
                                                .iter()
                                                .map(|h| h.uri.clone())
                                                .collect::<Vec<_>>(),)
                                                .to_variant(),
                                        )),
                                        Ok(Err(e)) => invocation.return_dbus_error(
                                            "io.github.FarisZR.GSeek.InferenceError",
                                            &e,
                                        ),
                                        Err(_) => invocation.return_dbus_error(
                                            "io.github.FarisZR.GSeek.Unavailable",
                                            "Search worker stopped",
                                        ),
                                    }
                                });
                            }
                            "GetResultMetas" => {
                                let (ids,) = parameters
                                    .get::<(Vec<String>,)>()
                                    .context("Invalid identifiers")?;
                                let store = Store::open(&spaths.database())?;
                                let settings = spaths.load()?;
                                let mut metas = vec![];
                                for id in ids.iter().take(100) {
                                    if let Some(file) = store.file(id)? {
                                        if !settings.allows(&file.path) {
                                            continue;
                                        }
                                        let icon = integration::icon_for(&file.path, &file.mime);
                                        let mut meta = std::collections::HashMap::<
                                            String,
                                            glib::Variant,
                                        >::new(
                                        );
                                        meta.insert("id".into(), id.to_variant());
                                        meta.insert("name".into(), file.name.to_variant());
                                        if let Some(icon) = icon.serialize() {
                                            meta.insert("icon".into(), icon);
                                        }
                                        let description = file
                                            .path
                                            .parent()
                                            .unwrap_or(&file.path)
                                            .to_string_lossy()
                                            .to_string();
                                        meta.insert("description".into(), description.to_variant());
                                        metas.push(meta);
                                    }
                                }
                                invocation.return_value(Some(&(metas,).to_variant()));
                            }
                            "ActivateResult" => {
                                let (uri, _, _) = parameters
                                    .get::<(String, Vec<String>, u32)>()
                                    .context("Invalid result")?;
                                integration::open_result(&spaths, &uri)?;
                                invocation.return_value(None);
                            }
                            "LaunchSearch" => {
                                let (terms, _) = parameters
                                    .get::<(Vec<String>, u32)>()
                                    .context("Invalid search")?;
                                let launcher = launcher()?;
                                std::process::Command::new(launcher)
                                    .arg("--query")
                                    .arg(terms.join(" "))
                                    .spawn()?;
                                invocation.return_value(None);
                            }
                            _ => bail!("Unknown search-provider method"),
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        errors.return_dbus_error(
                            "io.github.FarisZR.GSeek.Error",
                            &format!("{error:#}"),
                        );
                    }
                },
            )
            .build()
            .expect("Cannot export the SearchProvider2 interface");
        let info = gio::DBusNodeInfo::for_xml(CONTROL_XML)
            .unwrap()
            .interfaces()[0]
            .clone();
        let paths = startup_paths.clone();
        let state = startup_shared.clone();
        let tx = startup_tx.clone();
        let shutdown = main_loop.clone();
        let queue = search.clone();
        connection
            .register_object(CONTROL_PATH, &info)
            .method_call(
                move |_conn, _sender, _path, _iface, method, parameters, invocation| {
                    // Synchronous control methods are tiny metadata operations; inference and discovery run elsewhere.
                    let result = (|| -> Result<Option<glib::Variant>> {
                        match method {
                            "Status" => Ok(Some(
                                (serde_json::to_string(&*state.lock().unwrap())?,).to_variant(),
                            )),
                            "Settings" => {
                                Ok(Some((serde_json::to_string(&paths.load()?)?,).to_variant()))
                            }
                            "Activity" => Ok(Some(
                                (serde_json::to_string(
                                    &Store::open(&paths.database())?.activity()?,
                                )?,)
                                    .to_variant(),
                            )),
                            "Configure" => {
                                let (json,) = parameters
                                    .get::<(String,)>()
                                    .context("Invalid configuration")?;
                                if json.len() > 65536 {
                                    bail!("Configuration is too large");
                                }
                                let settings: Settings = serde_json::from_str(&json)?;
                                paths.save(&settings)?;
                                state.lock().unwrap().paused = settings.paused;
                                tx.send(Command::Setup)?;
                                Ok(None)
                            }
                            "Search" => {
                                let (query, limit) =
                                    parameters.get::<(String, u32)>().context("Invalid query")?;
                                if !state.lock().unwrap().ready {
                                    bail!("{}", state.lock().unwrap().detail);
                                }
                                let response = queue.submit(query, limit as usize, false)?;
                                let invocation = invocation.clone();
                                glib::spawn_future_local(async move {
                                    match response.recv().await {
                                        Ok(Ok(hits)) => invocation.return_value(Some(
                                            &(serde_json::to_string(&hits).unwrap(),).to_variant(),
                                        )),
                                        Ok(Err(e)) => invocation.return_dbus_error(
                                            "io.github.FarisZR.GSeek.InferenceError",
                                            &e,
                                        ),
                                        Err(_) => invocation.return_dbus_error(
                                            "io.github.FarisZR.GSeek.Unavailable",
                                            "Search worker stopped",
                                        ),
                                    }
                                });
                                Ok(None)
                            }
                            "Command" => {
                                let (command, argument) = parameters
                                    .get::<(String, String)>()
                                    .context("Invalid command")?;
                                match command.as_str() {
                                    "pause" | "resume" => {
                                        let mut settings = paths.load()?;
                                        settings.paused = command == "pause";
                                        paths.save(&settings)?;
                                        state.lock().unwrap().paused = settings.paused;
                                        tx.send(Command::Wake)?;
                                    }
                                    "scan" => tx.send(Command::Scan)?,
                                    "retry" => tx.send(Command::Retry)?,
                                    "setup" => tx.send(Command::Setup)?,
                                    "reindex" => {
                                        tx.send(Command::Reindex(if argument.is_empty() {
                                            None
                                        } else {
                                            Some(argument.into())
                                        }))?
                                    }
                                    "enable-search" => integration::enable_search(&paths)?,
                                    "quit" => {
                                        tx.send(Command::Quit)?;
                                        shutdown.quit();
                                    }
                                    _ => bail!("Unknown command {command}"),
                                }
                                Ok(None)
                            }
                            _ => bail!("Unknown service method"),
                        }
                    })();
                    match result {
                        Ok(value) => {
                            if method != "Search" {
                                invocation.return_value(value.as_ref());
                            }
                        }
                        Err(e) => invocation
                            .return_dbus_error("io.github.FarisZR.GSeek.Error", &format!("{e:#}")),
                    }
                },
            )
            .build()
            .expect("Cannot export the control interface");
        // LocalSearch events prompt a reconciliation; a periodic full pass repairs missed signals.
        for sender in [
            "org.freedesktop.LocalSearch3",
            "org.freedesktop.Tracker3.Miner.Files",
        ] {
            let tx = startup_tx.clone();
            live_subscriptions
                .borrow_mut()
                .push(connection.subscribe_to_signal(
                    Some(sender),
                    None,
                    Some("GraphUpdated"),
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_| {
                        let _ = tx.send(Command::Scan);
                    },
                ));
        }
    }
    let pending_receiver = std::cell::RefCell::new(Some(rx));
    let stop = main_loop.clone();
    let owner = gio::bus_own_name_on_connection(
        &connection,
        BUS_NAME,
        gio::BusNameOwnerFlags::NONE,
        move |_, _| {
            let Some(rx) = pending_receiver.borrow_mut().take() else {
                return;
            };
            let worker_paths = worker_paths.clone();
            let worker_state = worker_state.clone();
            std::thread::spawn(move || {
                // Lower the indexer's scheduling priority without slowing interactive searches.
                unsafe {
                    libc::setpriority(
                        libc::PRIO_PROCESS,
                        libc::syscall(libc::SYS_gettid) as u32,
                        10,
                    );
                }
                if let Err(e) = index_loop(&worker_paths, &worker_state, rx) {
                    status(&worker_state, "error", format!("{e:#}"));
                }
            });
        },
        move |_, _| stop.quit(),
    );
    main_loop.run();
    gio::bus_unown_name(owner);
    let _ = tx.send(Command::Quit);
    Ok(())
}

pub fn launcher() -> Result<PathBuf> {
    Ok(std::env::var_os("GSEEK_LAUNCHER")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?))
}

struct SearchJob {
    query: String,
    limit: usize,
    generation: u64,
    shell: bool,
    reply: async_channel::Sender<Result<Vec<SearchHit>, String>>,
}
struct SearchQueue {
    sender: mpsc::SyncSender<SearchJob>,
    shell: Arc<AtomicU64>,
    ui: Arc<AtomicU64>,
}
impl SearchQueue {
    fn new(paths: &Paths) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<SearchJob>(32);
        let paths = paths.clone();
        let shell = Arc::new(AtomicU64::new(0));
        let ui = Arc::new(AtomicU64::new(0));
        let wshell = shell.clone();
        let wui = ui.clone();
        std::thread::spawn(move || {
            let mut runtime = None;
            let mut previous = String::new();
            while let Ok(job) = receiver.recv() {
                let generation = if job.shell { &wshell } else { &wui };
                if generation.load(Ordering::Relaxed) != job.generation {
                    let _ = job.reply.send_blocking(Ok(vec![]));
                    continue;
                }
                let result = (|| -> Result<Vec<SearchHit>> {
                    let settings = paths.load()?;
                    if !settings.configured {
                        bail!("Complete GSeek setup before searching");
                    }
                    let serialized = serde_json::to_string(&settings)?;
                    if serialized != previous {
                        runtime = Some(Runtime::new(&paths, &settings, true));
                        previous = serialized;
                    }
                    let runtime = runtime.as_mut().unwrap();
                    // Inference is required. There is intentionally no keyword-only fallback.
                    let vector = runtime.query(&job.query)?;
                    if generation.load(Ordering::Relaxed) != job.generation {
                        return Ok(vec![]);
                    }
                    Store::open(&paths.database())?.search(
                        &job.query,
                        &vector,
                        &runtime.fingerprint(),
                        &settings,
                        job.limit,
                    )
                })()
                .map_err(|e| format!("{e:#}"));
                let _ = job.reply.send_blocking(result);
            }
        });
        Self { sender, shell, ui }
    }
    fn submit(
        &self,
        query: String,
        limit: usize,
        shell: bool,
    ) -> Result<async_channel::Receiver<Result<Vec<SearchHit>, String>>> {
        if query.chars().count() > 4096 {
            bail!("Search query is too long");
        }
        let counter = if shell { &self.shell } else { &self.ui };
        let generation = counter.fetch_add(1, Ordering::Relaxed) + 1;
        let (reply, rx) = async_channel::bounded(1);
        self.sender
            .try_send(SearchJob {
                query,
                limit: limit.clamp(1, 200),
                generation,
                shell,
                reply,
            })
            .context("Search is busy; try again in a moment")?;
        Ok(rx)
    }
}

fn index_loop(paths: &Paths, shared: &Shared, receiver: mpsc::Receiver<Command>) -> Result<()> {
    let mut store = Store::open(&paths.database())?;
    let mut settings = paths.load()?;
    let mut runtime = None;
    let mut setup = settings.configured;
    let mut scan = true;
    let mut last_scan = Instant::now() - Duration::from_secs(301);
    let mut backoff = Instant::now();
    loop {
        while let Ok(command) = receiver.try_recv() {
            match command {
                Command::Quit => return Ok(()),
                Command::Setup => {
                    setup = true;
                    scan = true;
                    backoff = Instant::now();
                }
                Command::Scan => scan = true,
                Command::Retry => {
                    store.retry(None, true)?;
                    setup = true;
                    backoff = Instant::now();
                }
                Command::Reindex(p) => {
                    store.retry(p.as_deref(), false)?;
                    scan = true;
                }
                Command::Wake => (),
            }
        }
        settings = paths.load()?;
        shared.lock().unwrap().paused = settings.paused;
        if setup && Instant::now() >= backoff {
            let initialized = (|| -> Result<Runtime> {
                shared.lock().unwrap().ready = false;
                status(shared, "setup", "Preparing private, on-device search");
                if settings.external.is_none() {
                    let mut last = Instant::now() - Duration::from_secs(1);
                    model::ensure(paths, |name, done, total| {
                        if last.elapsed() > Duration::from_millis(150) || done == total {
                            let mut s = shared.lock().unwrap();
                            s.detail =
                                format!("{name} · {} / {} MB", done / 1_000_000, total / 1_000_000);
                            s.progress = if total > 0 {
                                done as f64 / total as f64
                            } else {
                                0.0
                            };
                            last = Instant::now();
                        }
                        Ok(())
                    })?;
                }
                status(shared, "setup", "Checking EmbeddingGemma 2");
                let mut runtime = Runtime::new(paths, &settings, false);
                runtime.validate()?;
                settings.configured = true;
                paths.save(&settings)?;
                Ok(runtime)
            })();
            match initialized {
                Ok(r) => {
                    shared.lock().unwrap().backend = r.backend.clone();
                    shared.lock().unwrap().ready = true;
                    runtime = Some(r);
                    setup = false;
                    scan = true;
                }
                Err(e) => {
                    status(shared, "error", format!("{e:#}"));
                    shared.lock().unwrap().ready = false;
                    backoff = Instant::now() + Duration::from_secs(30);
                }
            }
        }
        if !settings.configured && !setup {
            status(shared, "welcome", "Open GSeek to set up semantic search");
        } else if runtime.is_some() && !setup {
            if scan || last_scan.elapsed() >= Duration::from_secs(300) {
                // Coalesce LocalSearch's burst of GraphUpdated signals into one reconciliation.
                scan = false;
                status(
                    shared,
                    "discovering",
                    "Syncing file locations with GNOME LocalSearch",
                );
                store.prune_scope(&settings)?;
                let generation = store.start_scan()?;
                match discovery::reconcile(&settings, |files| {
                    for file in files {
                        store.discover(&file, generation)?;
                    }
                    Ok(())
                }) {
                    Ok(_) => {
                        store.finish_scan(generation)?;
                        shared.lock().unwrap().last_scan = crate::store::now();
                    }
                    Err(e) => {
                        status(shared, "error", format!("{e:#}"));
                        store.log("LocalSearch", "failed", &format!("{e:#}"))?;
                    }
                }
                last_scan = Instant::now();
            }
            shared.lock().unwrap().counts = store.counts()?;
            if !settings.paused
                && let Some(file) = store.next_pending()?
            {
                shared.lock().unwrap().current = file.path.to_string_lossy().into_owned();
                status(shared, "indexing", format!("Reading {}", file.name));
                let result = (|| -> Result<()> {
                    if discovery::record_from_uri(&file.uri, &settings)?.is_none() {
                        bail!("File moved or is outside the current scope");
                    }
                    let hash = extract::content_hash(&file.path)?;
                    let runtime = runtime.as_mut().unwrap();
                    let content = format!("{}:{hash}", runtime.fingerprint());
                    if store.cached(&content, &runtime.fingerprint())? {
                        store.complete(&file.uri, &content)?;
                        return Ok(());
                    }
                    let mut chunks = vec![];
                    extract::read(&file, &settings, &paths.cache, |part| {
                        if shared.lock().unwrap().paused {
                            bail!("Indexing paused");
                        }
                        let vector = runtime.embed(&part.input)?;
                        chunks.push(StoredChunk {
                            text: part.text,
                            vector: Some(vector),
                            page: part.page,
                            seconds: part.seconds,
                        });
                        Ok(())
                    })?;
                    // Do not commit embeddings if the file changed under the reader.
                    let current = discovery::record_from_uri(&file.uri, &settings)?
                        .context("File was removed during indexing")?;
                    if current.stamp != file.stamp {
                        store.discover(&current, store.start_scan()?)?;
                        bail!("File changed during indexing; it will be retried");
                    }
                    if chunks.is_empty() {
                        bail!("No searchable content was found");
                    }
                    store.save_content(&content, &runtime.fingerprint(), &chunks)?;
                    store.complete(&file.uri, &content)?;
                    Ok(())
                })();
                if let Err(e) = result
                    && !shared.lock().unwrap().paused
                {
                    store.mark(&file.uri, "failed", &format!("{e:#}"))?;
                }
                shared.lock().unwrap().counts = store.counts()?;
                continue;
            }
            if settings.paused {
                status(
                    shared,
                    "paused",
                    "Indexing is paused. Search is still available.",
                );
            } else if shared.lock().unwrap().state != "error" {
                status(shared, "ready", "Your files are ready to search");
            }
            shared.lock().unwrap().current.clear();
            runtime.as_mut().unwrap().release_idle();
        }
        match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(Command::Quit) => return Ok(()),
            Ok(Command::Setup) => {
                setup = true;
                backoff = Instant::now();
            }
            Ok(Command::Scan) => scan = true,
            Ok(Command::Retry) => {
                store.retry(None, true)?;
                setup = true;
                backoff = Instant::now();
            }
            Ok(Command::Reindex(p)) => {
                store.retry(p.as_deref(), false)?;
                scan = true;
            }
            Ok(Command::Wake) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
    }
}

pub fn call(method: &str, parameters: Option<&glib::Variant>) -> Result<glib::Variant> {
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    Ok(connection.call_sync(
        Some(BUS_NAME),
        CONTROL_PATH,
        CONTROL_IFACE,
        method,
        parameters,
        None,
        gio::DBusCallFlags::NONE,
        120000,
        gio::Cancellable::NONE,
    )?)
}
pub fn read_status() -> Result<Status> {
    let value = call("Status", None)?;
    let (json,) = value
        .get::<(String,)>()
        .context("Invalid status response")?;
    Ok(serde_json::from_str(&json)?)
}
pub fn read_activity() -> Result<Vec<Activity>> {
    let value = call("Activity", None)?;
    let (json,) = value
        .get::<(String,)>()
        .context("Invalid activity response")?;
    Ok(serde_json::from_str(&json)?)
}
pub fn command(name: &str, argument: &str) -> Result<()> {
    call("Command", Some(&(name, argument).to_variant()))?;
    Ok(())
}
pub fn search(query: &str, limit: u32) -> Result<Vec<SearchHit>> {
    let value = call("Search", Some(&(query, limit).to_variant()))?;
    let (json,) = value
        .get::<(String,)>()
        .context("Invalid search response")?;
    Ok(serde_json::from_str(&json)?)
}
