"""Scope an installed Flatpak to this disposable test session only."""
import os
from pathlib import Path
import subprocess
import shutil

root = Path(os.environ['GSEEK_TEST_ROOT'])
app = 'com.fariszr.gseek'
# Per-test overrides never request new production permissions. Remove on exit.
subprocess.run(['flatpak', 'override', '--user', f'--filesystem={root}',
    f'--env=XDG_DATA_HOME={root}/data', f'--env=XDG_CONFIG_HOME={root}/config',
    f'--env=XDG_CACHE_HOME={root}/cache', app], check=True)
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
