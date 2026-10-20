//! Replace the running desktop and provider through their session-bus contracts.
use crate::{APP_ID, BUS_NAME, CONTROL_IFACE, CONTROL_PATH};
use anyhow::{Context, Result, bail};
use glib::variant::ToVariant;
use std::{
    collections::HashMap,
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
            "/com/fariszr/gseek",
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

fn activate(connection: &gio::DBusConnection, name: &str) -> Result<()> {
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "StartServiceByName",
            Some(&(name, 0u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            10000,
            gio::Cancellable::NONE,
        )
        .with_context(|| {
            format!("Cannot restart {name}. Reinstall GSeek to repair desktop activation.")
        })?;
    Ok(())
}

pub fn restart(background: bool) -> Result<()> {
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    let visible = stop_window(&connection)?;
    stop_service(&connection)?;
    activate(&connection, BUS_NAME)?;
    if visible || !background {
        activate(&connection, APP_ID)?;
    }
    Ok(())
}
