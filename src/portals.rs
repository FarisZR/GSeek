//! Desktop integration through the host's documented D-Bus APIs.
use anyhow::{Context, Result, bail};
use glib::variant::{ObjectPath, ToVariant};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
type Options = HashMap<String, glib::Variant>;

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

fn installed_update(value: &glib::Variant) -> bool {
    let Some((info,)) = value.get::<(Options,)>() else {
        return false;
    };
    let running = info.get("running-commit").and_then(|v| v.get::<String>());
    let local = info.get("local-commit").and_then(|v| v.get::<String>());
    matches!((running, local), (Some(running), Some(local)) if !running.is_empty() && !local.is_empty() && running != local)
}

/// Software manages installation. Only restart after a new deployment exists
/// locally; a remote update notification alone must not interrupt indexing.
pub fn watch_installed_updates(
    bus: &gio::DBusConnection,
) -> std::rc::Rc<std::cell::RefCell<Option<gio::SignalSubscription>>> {
    let subscription = std::rc::Rc::new(std::cell::RefCell::new(None));
    if std::env::var_os("FLATPAK_ID").is_none() {
        return subscription;
    }
    let keep = subscription.clone();
    let bus = bus.clone();
    glib::spawn_future_local(async move {
        let monitor = bus
            .call_future(
                Some("org.freedesktop.portal.Flatpak"),
                "/org/freedesktop/portal/Flatpak",
                "org.freedesktop.portal.Flatpak",
                "CreateUpdateMonitor",
                Some(&(Options::new(),).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                10000,
            )
            .await;
        let handle = match monitor {
            Ok(value) => value.get::<(ObjectPath,)>().map(|(path,)| path),
            Err(error) => {
                eprintln!("GSeek: update monitor unavailable: {error}");
                return;
            }
        };
        let Some(handle) = handle else {
            return;
        };
        let restarting = std::cell::Cell::new(false);
        *keep.borrow_mut() = Some(bus.subscribe_to_signal(
            Some("org.freedesktop.portal.Flatpak"),
            Some("org.freedesktop.portal.Flatpak.UpdateMonitor"),
            Some("UpdateAvailable"),
            Some(handle.as_str()),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if installed_update(signal.parameters) && !restarting.replace(true) {
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
                        Err(error) => {
                            restarting.set(false);
                            eprintln!("GSeek: cannot apply installed update: {error}");
                        }
                    }
                }
            },
        ));
    });
    subscription
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_requires_an_installed_deployment_change() {
        for (running, local, remote, expected) in [
            ("a", "a", "b", false),
            ("a", "b", "b", true),
            ("", "b", "b", false),
        ] {
            let info = Options::from([
                ("running-commit".into(), running.to_variant()),
                ("local-commit".into(), local.to_variant()),
                ("remote-commit".into(), remote.to_variant()),
            ]);
            assert_eq!(installed_update(&(info,).to_variant()), expected);
        }
        assert!(!installed_update(&().to_variant()));
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
