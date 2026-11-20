//! Desktop integration through the host's documented D-Bus APIs.
use anyhow::{Context, Result, bail};
use gio::prelude::*;
use glib::variant::ToVariant;
use std::{
    collections::{HashMap, HashSet},
    ffi::{CString, OsStr},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
type Options = HashMap<String, glib::Variant>;

pub fn is_document_portal_path(path: &Path) -> bool {
    path.starts_with(glib::user_runtime_dir().join("doc")) || path.starts_with("/run/flatpak/doc")
}

pub fn resolve_folder_path(path: PathBuf) -> Result<PathBuf> {
    let path = if is_document_portal_path(&path) {
        document_host_path(&path).with_context(|| {
            format!("Could not find the original location of {}", path.display())
        })?
    } else {
        path
    };
    if path.to_str().is_none() {
        bail!("Folder paths must be valid UTF-8");
    }
    Ok(path)
}

fn document_host_path(path: &Path) -> Result<PathBuf> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    // The portal provides this attribute on exports and their descendants.
    let mut value = [0u8; libc::PATH_MAX as usize];
    // SAFETY: Both strings are NUL-terminated and the output buffer is writable.
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"user.document-portal.host-path".as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    if size < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let bytes = &value[..size as usize];
    let path = PathBuf::from(OsStr::from_bytes(bytes));
    if bytes.contains(&0)
        || !path.is_absolute()
        || path == Path::new("/")
        || path.components().any(|c| c == Component::ParentDir)
        || is_document_portal_path(&path)
    {
        bail!("The document portal returned an invalid host path");
    }
    Ok(path)
}

/// Recover saved aliases where the original export is still available.
pub fn migrate_folder_paths(folders: &mut Vec<PathBuf>) -> bool {
    let mut changed = false;
    for path in folders.iter_mut() {
        if is_document_portal_path(path)
            && let Ok(host) = resolve_folder_path(path.clone())
        {
            *path = host;
            changed = true;
        }
    }
    if changed {
        let mut seen = HashSet::new();
        folders.retain(|path| seen.insert(path.clone()));
    }
    changed
}

pub fn open_search_settings() -> Result<()> {
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    bus.call_sync(
        Some("org.gnome.Settings"),
        "/org/gnome/Settings",
        "org.freedesktop.Application",
        "ActivateAction",
        Some(
            &(
                "launch-panel",
                vec![("search", Vec::<glib::Variant>::new()).to_variant()],
                Options::new(),
            )
                .to_variant(),
        ),
        None,
        gio::DBusCallFlags::NONE,
        10000,
        gio::Cancellable::NONE,
    )?;
    Ok(())
}

/// Called from the settings worker, never from GTK's main thread. Subscribe
/// before requesting permission: portals may answer before the method returns.
pub fn request_background() -> Result<()> {
    if std::env::var_os("FLATPAK_ID").is_none() {
        return Ok(());
    }
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    background_request(&bus)
}

fn background_request(bus: &gio::DBusConnection) -> Result<()> {
    let context = glib::MainContext::new();
    context.with_thread_default(|| -> Result<()> {
        let token = format!("gseek_{}", std::process::id());
        let sender = bus.unique_name().context("Session bus has no unique name")?
            .trim_start_matches(':').replace('.', "_");
        let handle = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
        let (tx, rx) = std::sync::mpsc::channel();
        let subscription = bus.subscribe_to_signal(Some(DESKTOP), Some("org.freedesktop.portal.Request"),
            Some("Response"), Some(&handle), None, gio::DBusSignalFlags::NONE,
            move |signal| { let _ = tx.send(signal.parameters.clone()); });
        let options = Options::from([
            ("handle_token".into(), token.to_variant()),
            ("reason".into(), "Keep your search index up to date and answer searches in Activities.".to_variant()),
            ("autostart".into(), true.to_variant()),
            ("commandline".into(), vec!["gseek", "service"].to_variant()),
            ("dbus-activatable".into(), false.to_variant()),
        ]);
        let result = (|| {
            bus.call_sync(Some(DESKTOP), DESKTOP_PATH, "org.freedesktop.portal.Background", "RequestBackground",
                Some(&("", options).to_variant()), None, gio::DBusCallFlags::NONE, 10000, gio::Cancellable::NONE)?;
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                while context.pending() { context.iteration(false); }
                if let Ok(value) = rx.try_recv() {
                    let (response, results) = value.get::<(u32, Options)>().context("Invalid background permission response")?;
                    if response != 0 || results.get("background").and_then(|v| v.get::<bool>()) != Some(true) {
                        bail!("Background access was declined. Allow GSeek to run in the background, then try setup again.");
                    }
                    if results.get("autostart").and_then(|v| v.get::<bool>()) != Some(true) {
                        bail!("Automatic startup was declined. Allow GSeek to start at login, then try setup again.");
                    }
                    return Ok(());
                }
                if Instant::now() >= deadline { bail!("Background permission request timed out. Try setup again."); }
                std::thread::sleep(Duration::from_millis(10));
            }
        })();
        drop(subscription);
        if result.is_err() {
            let _ = bus.call_sync(Some(DESKTOP), &handle, "org.freedesktop.portal.Request", "Close",
                None, None, gio::DBusCallFlags::NONE, 1000, gio::Cancellable::NONE);
        }
        result
    })?
}

/// Flatpak marks a superseded running deployment at /app/.updated. Watching
/// that marker applies installed updates immediately, without network polling
/// or access to the host's installation directories.
pub fn watch_installed_updates() -> Option<gio::FileMonitor> {
    std::env::var_os("FLATPAK_ID")?;
    match watch_update_marker(std::path::Path::new("/app/.updated"), || {
        let result = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe)
                .args(["restart", "--background"])
                .spawn()
        });
        match result {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(error) => eprintln!("GSeek: cannot apply installed update: {error}"),
        }
    }) {
        Ok(monitor) => Some(monitor),
        Err(error) => {
            eprintln!("GSeek: update monitor unavailable: {error:#}");
            None
        }
    }
}

fn watch_update_marker(
    path: &std::path::Path,
    restart: impl Fn() + 'static,
) -> Result<gio::FileMonitor> {
    let file = gio::File::for_path(path);
    let monitor = file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)?;
    let once = std::rc::Rc::new(std::cell::Cell::new(false));
    let check = std::rc::Rc::new(move |file: &gio::File| {
        if file.query_exists(gio::Cancellable::NONE) && !once.replace(true) {
            restart();
        }
    });
    let changed = check.clone();
    monitor.connect_changed(move |_, file, _, _| changed(file));
    check(&file); // Also cover an update installed just before the monitor started.
    Ok(monitor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::variant::ObjectPath;

    #[test]
    fn document_host_path_reads_the_exact_folder_location() -> Result<()> {
        let root = tempfile::tempdir()?;
        let alias = root.path().join("exported folder");
        std::fs::create_dir(&alias)?;
        assert!(document_host_path(&alias).is_err());

        let filename = CString::new(alias.as_os_str().as_bytes())?;
        let set_host_path = |value: &[u8]| -> Result<()> {
            // SAFETY: The filename and attribute name are NUL-terminated, and
            // the value is readable for the specified length.
            let status = unsafe {
                libc::setxattr(
                    filename.as_ptr(),
                    c"user.document-portal.host-path".as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            };
            if status < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        };
        let host = Path::new("/home/test/Documents/Parent/Private ü");
        set_host_path(host.as_os_str().as_bytes())?;
        assert_eq!(document_host_path(&alias)?, host);
        set_host_path(b"/home/test/Documents/../Pictures")?;
        assert!(document_host_path(&alias).is_err());
        Ok(())
    }

    #[test]
    fn installed_deployment_marker_triggers_one_restart() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join(".updated");
        let context = glib::MainContext::new();
        context.with_thread_default(|| -> Result<()> {
            let count = std::rc::Rc::new(std::cell::Cell::new(0));
            let calls = count.clone();
            let _monitor = watch_update_marker(&path, move || calls.set(calls.get() + 1))?;
            assert_eq!(count.get(), 0);
            std::fs::write(&path, "")?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while count.get() == 0 && Instant::now() < deadline {
                context.iteration(false);
            }
            assert_eq!(count.get(), 1);
            std::fs::write(&path, "installed")?;
            while context.pending() {
                context.iteration(false);
            }
            assert_eq!(count.get(), 1);
            let already_installed = std::rc::Rc::new(std::cell::Cell::new(false));
            let called = already_installed.clone();
            let _monitor = watch_update_marker(&path, move || called.set(true))?;
            assert!(already_installed.get());
            Ok(())
        })??;
        Ok(())
    }

    #[test]
    fn background_portal_handles_immediate_responses_and_denials() -> Result<()> {
        use std::io::BufRead;
        struct Daemon(std::process::Child);
        impl Drop for Daemon {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut daemon = Daemon(
            std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .spawn()?,
        );
        let mut address = String::new();
        std::io::BufReader::new(daemon.0.stdout.take().unwrap()).read_line(&mut address)?;
        let connect = |address: &str| {
            gio::DBusConnection::for_address_sync(
                address.trim(),
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                    | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                None,
                gio::Cancellable::NONE,
            )
        };
        let bus = connect(&address)?;
        bus.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(DESKTOP, 0u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
        )?;
        let context = glib::MainContext::new();
        context.with_thread_default(|| -> Result<()> {
            let cases = std::rc::Rc::new(std::cell::RefCell::new(std::collections::VecDeque::from([
                (0u32, true, true), (0, false, true), (0, true, false), (1, false, false)
            ])));
            let responses = cases.clone();
            let info = gio::DBusNodeInfo::for_xml("<node><interface name='org.freedesktop.portal.Background'><method name='RequestBackground'><arg type='s' direction='in'/><arg type='a{sv}' direction='in'/><arg type='o' direction='out'/></method></interface></node>")?;
            let registration = bus.register_object(DESKTOP_PATH, &info.interfaces()[0])
                .method_call(move |connection, sender, _, _, method, parameters, invocation| {
                    assert_eq!(method, "RequestBackground");
                    let (_, options) = parameters.get::<(String, Options)>().unwrap();
                    assert_eq!(options["autostart"].get::<bool>(), Some(true));
                    assert_eq!(options["dbus-activatable"].get::<bool>(), Some(false));
                    assert_eq!(options["commandline"].get::<Vec<String>>().unwrap(), ["gseek", "service"]);
                    let sender = sender.unwrap();
                    let token = options["handle_token"].get::<String>().unwrap();
                    let handle = format!("/org/freedesktop/portal/desktop/request/{}/{token}", sender.trim_start_matches(':').replace('.', "_"));
                    let (code, background, autostart) = responses.borrow_mut().pop_front().unwrap();
                    let result = Options::from([("background".into(), background.to_variant()), ("autostart".into(), autostart.to_variant())]);
                    // Deliberately respond before returning the request handle.
                    connection.emit_signal(Some(sender), &handle, "org.freedesktop.portal.Request", "Response", Some(&(code, result).to_variant())).unwrap();
                    invocation.return_value(Some(&(ObjectPath::try_from(handle).unwrap(),).to_variant()));
                }).build()?;
            for expected in [true, false, false, false] {
                let client = connect(&address)?;
                let worker = std::thread::spawn(move || background_request(&client));
                let deadline = Instant::now() + Duration::from_secs(5);
                while !worker.is_finished() && Instant::now() < deadline {
                    while context.pending() { context.iteration(false); }
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(worker.is_finished(), "Portal response was lost");
                assert_eq!(worker.join().unwrap().is_ok(), expected);
            }
            assert!(cases.borrow().is_empty());
            bus.unregister_object(registration)?;
            Ok(())
        })??;
        Ok(())
    }
}
