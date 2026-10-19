"""A disposable real GNOME Shell integration check. Never use a user's session.

The test-only extension permits Shell's Eval/Screenshot APIs on this private
bus. It is not installed or distributed with GSeek.
"""
import json
import os
from pathlib import Path
import subprocess
import time

from gi.repository import Gio, GLib


def check_overview(bus, root, wait_for):
    assert os.environ["GSETTINGS_BACKEND"] == "keyfile", "Shell and GSeek need shared disposable settings"
    extension = root / "data/gnome-shell/extensions/gseek-ci@example.invalid"
    extension.mkdir(parents=True)
    (extension / "metadata.json").write_text(json.dumps({
        "uuid": extension.name, "name": "GSeek integration test", "description": "Private test bus only", "shell-version": ["48", "51"], "version": 1,
    }))
    (extension / "extension.js").write_text("import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';\nexport default class Test extends Extension { enable() { global.context.unsafe_mode = true; } disable() { global.context.unsafe_mode = false; } }\n")
    settings = Gio.Settings.new("org.gnome.shell")
    settings.set_strv("enabled-extensions", [extension.name])
    settings.set_boolean("disable-user-extensions", False)
    Gio.Settings.new("org.gnome.desktop.notifications").set_boolean("show-banners", False)
    Gio.Settings.new("org.gnome.desktop.interface").set_boolean("enable-animations", False)
    Gio.Settings.sync()
    runtime = root / "runtime"
    runtime.mkdir(mode=0o700, exist_ok=True)
    env = {**os.environ, "DBUS_SYSTEM_BUS_ADDRESS": os.environ["DBUS_SESSION_BUS_ADDRESS"], "XDG_RUNTIME_DIR": str(runtime), "WAYLAND_DISPLAY": "wayland-gseek-ci"}
    log = root / "shell.log"
    with log.open("w") as stream:
        shell = subprocess.Popen(["gnome-shell", "--headless", "--wayland", "--no-x11", "--wayland-display=wayland-gseek-ci", "--virtual-monitor=1280x800"], env=env, stdout=stream, stderr=subprocess.STDOUT)
        try:
            def evaluate(script):
                if shell.poll() is not None:
                    raise AssertionError(f"GNOME Shell exited: {log.read_text()[-6000:]}")
                return bus.call_sync("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "Eval", GLib.Variant("(s)", (script,)), None, Gio.DBusCallFlags.NONE, 10000, None).unpack()

            wait_for(lambda: evaluate("global.context.unsafe_mode && !Main.layoutManager._startingUp") == (True, "true"), timeout=60)
            success, result = evaluate("Main.overview.show(); Main.overview.searchEntry.set_text('the clause about pets in my lease'); true")
            assert success, result
            script = "(() => { const texts = []; const visit = a => { if (typeof a.get_text === 'function') texts.push(a.get_text()); for (const child of a.get_children()) visit(child); }; visit(global.stage); return texts; })()"

            def has_result():
                success, result = evaluate(script)
                return success and "Rental Agreement.txt" in result

            wait_for(has_result, timeout=60)
            # Shell creates its result actors before finishing their allocation.
            settled = time.monotonic()
            wait_for(lambda: time.monotonic() - settled > 1)
            output = Path(os.environ.get("GSEEK_SCREENSHOT_DIR", root))
            output.mkdir(parents=True, exist_ok=True)
            success, filename = bus.call_sync("org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot", "org.gnome.Shell.Screenshot", "Screenshot", GLib.Variant("(bbs)", (False, False, str(output / "overview.png"))), None, Gio.DBusCallFlags.NONE, 10000, None).unpack()
            assert success and Path(filename).is_file(), "GNOME did not capture its overview"
        except Exception as error:
            raise AssertionError(f"{error}\nGNOME Shell log:\n{log.read_text()[-6000:]}") from error
        finally:
            shell.terminate()
            try:
                shell.wait(timeout=10)
            except subprocess.TimeoutExpired:
                shell.kill()
                shell.wait()
