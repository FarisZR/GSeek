#!/usr/bin/env python3
"""Install, reinstall, and uninstall this exact build through Homebrew's Linux cask lifecycle."""
import hashlib
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("render_cask", ROOT / "scripts/render-cask.py")
cask = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cask)


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, **kwargs)


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
        gui = subprocess.run(["xvfb-run", "-a", "dbus-run-session", "--", "timeout", "3", "gseek"], text=True, capture_output=True)
        assert gui.returncode == 124, gui.stderr
        assert "CRITICAL" not in gui.stderr and "symbol lookup error" not in gui.stderr, gui.stderr
        data = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share"))
        config = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
        desktop = data / "applications/io.github.FarisZR.GSeek.desktop"
        service = data / "dbus-1/services/io.github.FarisZR.GSeek.Service.service"
        provider = Path("/usr/local/share/gnome-shell/search-providers/io.github.FarisZR.GSeek.ini")
        autostart = config / "autostart/io.github.FarisZR.GSeek.desktop"
        for path in [desktop, service, provider, autostart]:
            assert path.is_file(), path
        assert "/.local/share/gseek-app/bin/gseek" in desktop.read_text()
        icon = Path(next(line[5:] for line in desktop.read_text().splitlines() if line.startswith("Icon=")))
        assert icon.is_absolute() and icon.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
        sentinel = data / "gseek/keep-index-data.txt"
        sentinel.write_text("Preserved across upgrades and uninstall")
        run("brew", "reinstall", "--cask", "fariszr/gseek-ci/gseek")
        run("gseek", "--version")
        run("brew", "uninstall", "--cask", "gseek")
        installed = False
        for path in [desktop, service, provider, autostart, icon]:
            assert not path.exists(), f"Stale registration: {path}"
        assert sentinel.is_file()
    finally:
        if installed:
            subprocess.run(["brew", "uninstall", "--cask", "gseek"], check=False)
        subprocess.run(["brew", "untap", "fariszr/gseek-ci"], check=False)
