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
#[serde(default)]
pub struct Status {
    pub discovery_error: String,
    pub discovering: bool,
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
    Retry,
    Reindex(Option<PathBuf>),
    Wake,
    Quit,
}

#[derive(Debug)]
enum DiscoveryCommand {
    Scan,
    Changed(Vec<i64>),
    Quit,
}

#[derive(Debug)]
enum IndexingInterruption {
    Changed,
    Yielded,
}
impl std::fmt::Display for IndexingInterruption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Changed => "File changed during indexing",
            Self::Yielded => "A smaller file is ready to index",
        })
    }
}
impl std::error::Error for IndexingInterruption {}
fn ensure_current(
    store: &Store,
    file: &crate::store::FileRecord,
    settings: &Settings,
    content: &str,
) -> Result<()> {
    match discovery::record_from_uri(&file.uri, settings)? {
        Some(current) if current.stamp == file.stamp => Ok(()),
        current => {
            store.discard_incomplete(content)?;
            if let Some(current) = current {
                store.refresh(&current)?;
            } else {
                store.remove_file(&file.uri)?;
            }
            Err(IndexingInterruption::Changed.into())
        }
    }
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
    let (discovery_tx, discovery_rx) = mpsc::channel();
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
                                        let description = settings.location_label(
                                            file.path.parent().unwrap_or(&file.path),
                                        );
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
        let control_discovery = discovery_tx.clone();
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
                                let mut settings: Settings = serde_json::from_str(&json)?;
                                settings.configured = true;
                                paths.save(&settings)?;
                                if !settings.search_enabled {
                                    integration::disable_search(&paths)?;
                                }
                                state.lock().unwrap().ready = false;
                                state.lock().unwrap().paused = settings.paused;
                                tx.send(Command::Setup)?;
                                control_discovery.send(DiscoveryCommand::Scan)?;
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
                                    "scan" => control_discovery.send(DiscoveryCommand::Scan)?,
                                    "retry" => tx.send(Command::Retry)?,
                                    "setup" => tx.send(Command::Setup)?,
                                    "reindex" => {
                                        tx.send(Command::Reindex(if argument.is_empty() {
                                            None
                                        } else {
                                            Some(argument.into())
                                        }))?
                                    }
                                    "enable-search" => {
                                        let mut s = paths.load()?;
                                        s.search_enabled = true;
                                        paths.save(&s)?;
                                        if state.lock().unwrap().ready {
                                            integration::enable_search(&paths)?;
                                        }
                                    }
                                    "disable-search" => {
                                        let mut s = paths.load()?;
                                        s.search_enabled = false;
                                        paths.save(&s)?;
                                        integration::disable_search(&paths)?;
                                    }
                                    "quit" => {
                                        tx.send(Command::Quit)?;
                                        control_discovery.send(DiscoveryCommand::Quit)?;
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
            let tx = discovery_tx.clone();
            live_subscriptions
                .borrow_mut()
                .push(connection.subscribe_to_signal(
                    Some(sender),
                    None,
                    Some("GraphUpdated"),
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |signal| {
                        let command =
                            if let Some(ids) = discovery::notification_ids(signal.parameters) {
                                DiscoveryCommand::Changed(ids)
                            } else {
                                DiscoveryCommand::Scan
                            };
                        let _ = tx.send(command);
                    },
                ));
        }
    }
    let pending_receiver = std::cell::RefCell::new(Some(rx));
    let pending_discovery = std::cell::RefCell::new(Some(discovery_rx));
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
            let discovery_rx = pending_discovery.borrow_mut().take().unwrap();
            let discovery_paths = worker_paths.clone();
            let discovery_state = worker_state.clone();
            std::thread::spawn(move || {
                if let Err(error) = discovery_loop(&discovery_paths, &discovery_state, discovery_rx)
                {
                    discovery_state.lock().unwrap().discovery_error = format!("{error:#}");
                }
            });
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
    let _ = discovery_tx.send(DiscoveryCommand::Quit);
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
            let mut runtime: Option<Runtime> = None;
            let mut previous = String::new();
            loop {
                let job = match receiver.recv_timeout(Duration::from_secs(30)) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(runtime) = runtime.as_mut() {
                            runtime.release_idle();
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
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

fn discovery_loop(
    paths: &Paths,
    shared: &Shared,
    receiver: mpsc::Receiver<DiscoveryCommand>,
) -> Result<()> {
    let store = Store::open(&paths.database())?;
    let mut full = true;
    let mut changed = std::collections::BTreeSet::<i64>::new();
    let mut last_full = Instant::now() - Duration::from_secs(301);
    let mut next = Instant::now();
    loop {
        while let Ok(command) = receiver.try_recv() {
            match command {
                DiscoveryCommand::Quit => return Ok(()),
                DiscoveryCommand::Scan => {
                    full = true;
                    next = Instant::now();
                }
                DiscoveryCommand::Changed(ids) => {
                    changed.extend(ids.into_iter().filter(|id| *id > 0))
                }
            }
        }
        let settings = paths.load()?;
        if settings.configured
            && Instant::now() >= next
            && (full || !changed.is_empty() || last_full.elapsed() >= Duration::from_secs(300))
        {
            shared.lock().unwrap().discovering = true;
            let snapshot = full || last_full.elapsed() >= Duration::from_secs(300);
            let result = (|| -> Result<()> {
                if snapshot {
                    store.prune_scope(&settings)?;
                    let generation = store.start_scan()?;
                    discovery::reconcile(&settings, |files| {
                        for file in files {
                            store.discover(&file, generation)?;
                        }
                        Ok(())
                    })?;
                    store.finish_scan(generation)?;
                    full = false;
                    changed.clear();
                    last_full = Instant::now();
                } else {
                    let ids: Vec<_> = changed.iter().take(4096).copied().collect();
                    let records = discovery::changed(&settings, &ids)?;
                    store.apply_changes(&ids, &records)?;
                    for id in ids {
                        changed.remove(&id);
                    }
                }
                Ok(())
            })();
            let mut status = shared.lock().unwrap();
            status.discovering = false;
            status.counts = store.counts()?;
            match result {
                Ok(()) => {
                    status.discovery_error.clear();
                    status.last_scan = crate::store::now();
                    next = Instant::now() + Duration::from_millis(750);
                }
                Err(error) => {
                    status.discovery_error = format!("{error:#}");
                    store.log("LocalSearch", "failed", &status.discovery_error)?;
                    next = Instant::now() + Duration::from_secs(30);
                }
            }
        }
        match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(DiscoveryCommand::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Ok(DiscoveryCommand::Scan) => {
                full = true;
                next = Instant::now();
            }
            Ok(DiscoveryCommand::Changed(ids)) => {
                changed.extend(ids.into_iter().filter(|id| *id > 0))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
    }
}

fn index_loop(paths: &Paths, shared: &Shared, receiver: mpsc::Receiver<Command>) -> Result<()> {
    let store = Store::open(&paths.database())?;
    let mut settings = paths.load()?;
    let mut runtime = None;
    let mut setup = settings.configured;
    let mut backoff = Instant::now();
    loop {
        while let Ok(command) = receiver.try_recv() {
            match command {
                Command::Quit => return Ok(()),
                Command::Setup => {
                    store.reconsider_skipped()?;
                    setup = true;
                    backoff = Instant::now();
                }
                Command::Retry => {
                    store.retry(None, true)?;
                    setup = true;
                    backoff = Instant::now();
                }
                Command::Reindex(p) => {
                    store.retry(p.as_deref(), false)?;
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
                let latest = paths.load()?;
                if latest.backend != settings.backend
                    || latest.external != settings.external
                    || latest.rocm_override != settings.rocm_override
                {
                    bail!(
                        "Inference settings changed; restarting setup with the new configuration"
                    );
                }
                settings = latest;
                store.activate_pipeline(&runtime.fingerprint())?;
                if settings.search_enabled {
                    integration::enable_search(paths)?;
                }
                Ok(runtime)
            })();
            match initialized {
                Ok(r) => {
                    shared.lock().unwrap().backend = r.backend.clone();
                    shared.lock().unwrap().ready = true;
                    runtime = Some(r);
                    setup = false;
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
            shared.lock().unwrap().counts = store.counts()?;
            if !settings.paused
                && let Some(file) = store.next_pending()?
            {
                if let Some(reason) = extract::skip_reason(&file, &settings) {
                    store.mark(&file.uri, "skipped", &reason)?;
                    continue;
                }
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
                        ensure_current(&store, &file, &paths.load()?, &content)?;
                        store.complete(&file.uri, &content)?;
                        return Ok(());
                    }
                    let completed = store.content_progress(&content)?;
                    if completed > 0 {
                        store.log(
                            &file.uri,
                            "resumed",
                            &format!("Resuming after {completed} completed parts"),
                        )?;
                    }
                    let mut index = 0;
                    extract::read(&file, &settings, &paths.cache, |part| {
                        if shared.lock().unwrap().paused {
                            bail!("Indexing paused");
                        }
                        let latest = paths.load()?;
                        ensure_current(&store, &file, &latest, &content)?;
                        if index < completed {
                            index += 1;
                            return Ok(());
                        }
                        let vector = runtime.embed(&part.input)?;
                        shared.lock().unwrap().backend = runtime.backend.clone();
                        ensure_current(&store, &file, &latest, &content)?;
                        store.checkpoint(
                            &content,
                            &runtime.fingerprint(),
                            index,
                            &StoredChunk {
                                text: part.text,
                                vector: Some(vector),
                                page: part.page,
                                seconds: part.seconds,
                            },
                        )?;
                        index += 1;
                        if store
                            .next_pending()?
                            .is_some_and(|next| next.uri != file.uri)
                        {
                            return Err(IndexingInterruption::Yielded.into());
                        }
                        Ok(())
                    })?;
                    ensure_current(&store, &file, &paths.load()?, &content)?;
                    if index == 0 {
                        bail!("No searchable content was found");
                    }
                    store.finish_content(&content)?;
                    store.complete(&file.uri, &content)?;
                    Ok(())
                })();
                let paused = shared.lock().unwrap().paused;
                if let Err(e) = result
                    && !paused
                {
                    if e.downcast_ref::<IndexingInterruption>().is_some() {
                        continue;
                    }
                    if e.downcast_ref::<crate::inference::InferenceError>()
                        .is_some()
                    {
                        status(shared, "error", format!("{e:#}"));
                        shared.lock().unwrap().ready = false;
                        store.log(&file.uri, "failed", &format!("{e:#}"))?;
                        setup = true;
                        backoff = Instant::now() + Duration::from_secs(30);
                    } else {
                        store.mark(&file.uri, "failed", &format!("{e:#}"))?;
                    }
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
                store.reconsider_skipped()?;
                setup = true;
                backoff = Instant::now();
            }
            Ok(Command::Retry) => {
                store.retry(None, true)?;
                setup = true;
                backoff = Instant::now();
            }
            Ok(Command::Reindex(p)) => {
                store.retry(p.as_deref(), false)?;
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

pub fn stop_running() {
    if let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        let _ = connection.call_sync(
            Some(BUS_NAME),
            CONTROL_PATH,
            CONTROL_IFACE,
            "Command",
            Some(&("quit", "").to_variant()),
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            2000,
            gio::Cancellable::NONE,
        );
    }
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
