"""Scope an installed Flatpak to this disposable test session only."""
import os
from pathlib import Path
import subprocess
import shutil
import sys

root = Path(os.environ['GSEEK_TEST_ROOT'])
app = 'com.fariszr.gseek'
# Per-test overrides never request new production permissions. Remove on exit.
permissions = [f'--filesystem={root}']
for variable in ['GSEEK_TEST_MODEL_CACHE', 'GSEEK_TEST_RUNTIME_CACHE']:
    if os.environ.get(variable):
        permissions.append('--filesystem=' + os.environ[variable])
subprocess.run(['flatpak', 'override', '--user', *permissions, app], check=True)
subprocess.run([sys.executable, str(Path(__file__).with_name('flatpak-data.py')), str(root)], check=True)
exports = Path(subprocess.check_output(['flatpak', 'info', '--user', '--show-location', app], text=True).strip()) / 'export/share'
# Use the actual installed/re-written exports, including D-Bus activation, desktop
# identity and search-provider registration. No test-generated provider files.
for relative in ['dbus-1/services', 'applications', 'gnome-shell/search-providers', 'icons']:
    source = exports / relative
    target = root / 'system/share' / relative
    if not source.is_dir():
        raise AssertionError(f'Flatpak did not export {relative}')
    shutil.copytree(source, target, dirs_exist_ok=True)
wrapper = root / 'gseek'
wrapper.write_text('#!/bin/sh\nexec /usr/bin/flatpak run --user com.fariszr.gseek "$@"\n')
wrapper.chmod(0o755)
