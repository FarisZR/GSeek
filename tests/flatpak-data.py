"""Connect a disposable account's Flatpak data to the test fixture.

Flatpak deliberately owns the XDG data/config/cache variables. Test their real
paths rather than attempting to override those reserved variables.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(sys.argv[1]).resolve()
state = root / 'flatpak-data-links.json'
if len(sys.argv) > 2 and sys.argv[2] == '--reset':
    if state.exists():
        for name, target in json.loads(state.read_text()).items():
            path = Path(name)
            if path.is_symlink() and path.resolve() == Path(target).resolve():
                path.unlink()
        state.unlink()
    raise SystemExit()
paths = subprocess.check_output(['flatpak', 'run', '--user', '--command=sh', 'com.fariszr.gseek', '-c',
    'printf "%s\\n%s\\n%s\\n" "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"'], text=True).splitlines()
assert len(paths) == 3
links = {str(Path(path) / 'gseek'): str(root / kind / 'gseek') for path, kind in zip(paths, ['data', 'config', 'cache'])}
for name in links:
    path = Path(name)
    if path.exists() or path.is_symlink():
        raise RuntimeError(f'GSeek data already exists at {path}. Run these tests in a disposable account.')
state.write_text(json.dumps(links))
for name, target in links.items():
    Path(target).mkdir(parents=True, exist_ok=True)
    Path(name).parent.mkdir(parents=True, exist_ok=True)
    Path(name).symlink_to(target, target_is_directory=True)
