#!/usr/bin/env python3
"""Install, reinstall, and uninstall this exact build through Homebrew's Linux cask lifecycle."""
import hashlib
import ast
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
if "GSEEK_BREW_TEST_SESSION" not in os.environ:
    # Keep the application alive in one desktop session through actual upgrades.
    with tempfile.TemporaryFile(mode="w+") as session_log:
        session = subprocess.run(["xvfb-run", "-a", "dbus-run-session", "--", sys.executable, __file__],
                  env={**os.environ, "GSEEK_BREW_TEST_SESSION": "1"}, text=True,
                  stdout=session_log, stderr=subprocess.STDOUT)
        session_log.seek(0)
        output = session_log.read()
    print(output, end="")
    if session.returncode:
        raise SystemExit(session.returncode)
    # D-Bus-activated replacements inherit the session log, not initial_window's
    # file descriptors. Check both, including every upgrade/restart/reinstall.
    assert "CRITICAL" not in output and "symbol lookup error" not in output, output
    raise SystemExit(0)
spec = importlib.util.spec_from_file_location("render_cask", ROOT / "scripts/render-cask.py")
cask = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cask)


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, **kwargs)


def wait_for(predicate):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise AssertionError("Timed out waiting for GSeek's desktop lifecycle")


def bus_owner(name):
    reply = subprocess.run(["gdbus", "call", "--session", "--dest", "org.freedesktop.DBus",
        "--object-path", "/org/freedesktop/DBus", "--method", "org.freedesktop.DBus.GetNameOwner", name],
        text=True, capture_output=True)
    return ast.literal_eval(reply.stdout)[0] if reply.returncode == 0 else None


def activate(action):
    run("gdbus", "call", "--session", "--dest", "io.github.FarisZR.GSeek",
        "--object-path", "/io/github/FarisZR/GSeek", "--method", "org.gtk.Actions.Activate",
        action, "[]", "{}")


arch = "aarch64" if os.uname().machine == "aarch64" else "x86_64"
archive = ROOT / f"dist/gseek-linux-{arch}.tar.gz"
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
with tempfile.TemporaryDirectory(prefix="gseek-tap-") as temporary:
    tap = Path(temporary)
    (tap / "Casks").mkdir()
    (tap / "Casks/gseek.rb").write_text(cask.render("0.1.3", archive.as_uri(), digest, digest))
    run("git", "init", str(tap))
    run("git", "-C", str(tap), "add", "Casks/gseek.rb")
    run("git", "-C", str(tap), "-c", "user.name=GSeek CI", "-c", "user.email=ci@example.invalid", "commit", "-m", "Test current GSeek build")
    run("brew", "tap", "--custom-remote", "fariszr/gseek-ci", str(tap))
    installed = False
    try:
        run("brew", "trust", "fariszr/gseek-ci")
        dependencies = subprocess.check_output(["brew", "deps", "--cask", "fariszr/gseek-ci/gseek"], text=True).strip()
        assert not dependencies, f"GSeek must reuse this host's OpenSSL and readers; requested: {dependencies}"
        run("brew", "install", "--cask", "fariszr/gseek-ci/gseek")
        installed = True
        run("gseek", "--version")
        gui_log = tap / "gui.log"
        with gui_log.open("w") as output:
            initial_window = subprocess.Popen(["gseek"], stdout=output, stderr=output)
        app_id = "io.github.FarisZR.GSeek"
        service_id = app_id + ".Service"
        wait_for(lambda: bus_owner(app_id) and bus_owner(service_id))
        data = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share"))
        config = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
        desktop = data / "applications/io.github.FarisZR.GSeek.desktop"
        service = data / "dbus-1/services/io.github.FarisZR.GSeek.Service.service"
        application = data / "dbus-1/services/io.github.FarisZR.GSeek.service"
        provider = Path("/usr/local/share/gnome-shell/search-providers/io.github.FarisZR.GSeek.ini")
        autostart = config / "autostart/io.github.FarisZR.GSeek.desktop"
        for path in [desktop, service, application, provider, autostart]:
            assert path.is_file(), path
        assert "/.local/share/gseek-app/bin/gseek" in desktop.read_text()
        icon = Path(next(line[5:] for line in desktop.read_text().splitlines() if line.startswith("Icon=")))
        assert icon.is_absolute() and icon.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
        sentinel = data / "gseek/keep-index-data.txt"
        sentinel.write_text("Preserved across upgrades and uninstall")
        def assert_replaced(operation):
            previous = {name: bus_owner(name) for name in [app_id, service_id]}
            operation()
            for name in previous:
                wait_for(lambda: bus_owner(name) and bus_owner(name) != previous[name])
            assert sentinel.read_text() == "Preserved across upgrades and uninstall"
        cask_file = Path(subprocess.check_output(["brew", "--repo", "fariszr/gseek-ci"], text=True).strip()) / "Casks/gseek.rb"
        cask_file.write_text(cask.render("0.1.4", archive.as_uri(), digest, digest))
        assert_replaced(lambda: run("brew", "upgrade", "--cask", "fariszr/gseek-ci/gseek"))
        initial_window.wait(timeout=10)
        assert_replaced(lambda: activate("restart"))
        assert_replaced(lambda: run("brew", "reinstall", "--cask", "fariszr/gseek-ci/gseek"))
        run("gseek", "--version")
        activate("quit")
        wait_for(lambda: not bus_owner(app_id))
        output = gui_log.read_text()
        assert "CRITICAL" not in output and "symbol lookup error" not in output, output
        run("brew", "uninstall", "--cask", "gseek")
        installed = False
        for path in [desktop, service, application, provider, autostart, icon]:
            assert not path.exists(), f"Stale registration: {path}"
        assert sentinel.is_file()
    finally:
        if installed:
            subprocess.run(["brew", "uninstall", "--cask", "gseek"], check=False)
        subprocess.run(["brew", "untap", "fariszr/gseek-ci"], check=False)
