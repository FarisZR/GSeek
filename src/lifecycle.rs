//! Replace the running desktop and provider through their session-bus contracts.
use crate::{APP_ID, BUS_NAME, CONTROL_IFACE, CONTROL_PATH, config::Paths, service};
use anyhow::{Context, Result, bail};
use glib::variant::ToVariant;
use std::{
    collections::HashMap,
    fs::OpenOptions,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn owner(connection: &gio::DBusConnection, name: &str) -> Result<Option<String>> {
    match connection.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetNameOwner",
        Some(&(name,).to_variant()),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        2000,
        gio::Cancellable::NONE,
    ) {
        Ok(value) => Ok(Some(
            value.get::<(String,)>().context("Invalid bus owner")?.0,
        )),
        Err(error)
            if gio::DBusError::remote_error(&error).as_deref()
                == Some("org.freedesktop.DBus.Error.NameHasNoOwner") =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn wait_for_exit(connection: &gio::DBusConnection, name: &str, previous: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while owner(connection, name)?.as_deref() == Some(previous) {
        if Instant::now() >= deadline {
            bail!("GSeek is still finishing its previous session. Try restarting again.");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn stop_window(connection: &gio::DBusConnection) -> Result<bool> {
    let Some(previous) = owner(connection, APP_ID)? else {
        return Ok(false);
    };
    // Address the captured owner so a concurrent launch cannot receive this quit.
    // The existing quit action closes the window after pending preference saves.
    connection
        .call_sync(
            Some(&previous),
            "/io/github/FarisZR/GSeek",
            "org.gtk.Actions",
            "Activate",
            Some(
                &(
                    "quit",
                    Vec::<glib::Variant>::new(),
                    HashMap::<String, glib::Variant>::new(),
                )
                    .to_variant(),
            ),
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            2000,
            gio::Cancellable::NONE,
        )
        .context("Cannot close the previous GSeek window")?;
    wait_for_exit(connection, APP_ID, &previous)?;
    Ok(true)
}

pub fn stop_service(connection: &gio::DBusConnection) -> Result<()> {
    if let Some(previous) = owner(connection, BUS_NAME)? {
        connection
            .call_sync(
                Some(&previous),
                CONTROL_PATH,
                CONTROL_IFACE,
                "Command",
                Some(&("quit", "").to_variant()),
                None,
                gio::DBusCallFlags::NO_AUTO_START,
                2000,
                gio::Cancellable::NONE,
            )
            .context("Cannot stop the previous GSeek search service")?;
        wait_for_exit(connection, BUS_NAME, &previous)?;
    }
    Ok(())
}

fn launch(paths: &Paths, launcher: &Path, connection: &gio::DBusConnection) -> Result<()> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(paths.cache.join("session.log"))?;
    let mut command = Command::new(launcher);
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Detach from Homebrew's terminal/process group without a shell or a daemon dependency.
    command.process_group(0);
    let mut child = command
        .spawn()
        .context("Cannot launch the installed GSeek version")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while owner(connection, APP_ID)?.is_none() {
        if let Some(status) = child.try_wait()? {
            bail!(
                "GSeek could not reopen ({status}). See {}",
                paths.cache.join("session.log").display()
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "GSeek did not reopen. See {}",
                paths.cache.join("session.log").display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

pub fn restart(paths: &Paths) -> Result<()> {
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    let launcher = service::launcher()?;
    stop_window(&connection)?;
    stop_service(&connection)?;
    // The new window activates the provider through its registered D-Bus service.
    launch(paths, &launcher, &connection)
}

pub fn integrate(paths: &Paths, launcher: &Path) -> Result<()> {
    // Installation also works outside a desktop session (for example over SSH).
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok();
    let reopen = if let Some(connection) = &connection {
        let reopen = stop_window(connection)?;
        stop_service(connection)?;
        reopen
    } else {
        false
    };
    crate::integration::install(paths, launcher)?;
    if let Some(connection) = connection {
        if paths.load()?.configured {
            // D-Bus starts the installed provider in the desktop session environment,
            // rather than leaving a service inside Homebrew's installation sandbox.
            connection
                .call_sync(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "StartServiceByName",
                    Some(&(BUS_NAME, 0u32).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    10000,
                    gio::Cancellable::NONE,
                )
                .context("GSeek was installed, but its search service could not restart")?;
        }
        if reopen {
            launch(paths, launcher, &connection)?;
        }
    }
    Ok(())
}
