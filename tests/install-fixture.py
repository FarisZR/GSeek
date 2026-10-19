"""Install static development activation into the disposable integration session."""
import os
from pathlib import Path
import shutil

source = Path(__file__).resolve().parents[1] / 'data'
root = Path(os.environ['GSEEK_TEST_ROOT'])
data = root / 'data'
binary = str(Path(os.environ['GSEEK_BINARY']).resolve())
# D-Bus Exec uses its own quoting rules. The test path is controlled, not user input.
assert not any(c in binary for c in '\n\r"\\')
for name in ['com.fariszr.gseek', 'com.fariszr.gseek.Service']:
    target = data / f'dbus-1/services/{name}.service'
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text((source / f'{name}.service').read_text().replace('/app/bin/gseek', f'"{binary}"'))
for relative, original in [
    ('applications/com.fariszr.gseek.desktop', 'com.fariszr.gseek.desktop'),
    ('icons/hicolor/512x512/apps/com.fariszr.gseek.png', 'icons/512x512/apps/com.fariszr.gseek.png'),
]:
    target = data / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source / original, target)
provider = root / 'system/share/gnome-shell/search-providers/com.fariszr.gseek.ini'
provider.parent.mkdir(parents=True, exist_ok=True)
shutil.copyfile(source / 'com.fariszr.gseek.ini', provider)
