#!/usr/bin/env python3
"""Create the public installer and immutable inputs for repository signing."""
import argparse
import base64
from pathlib import Path
import tarfile


def assemble(bundles, output):
    output.mkdir(parents=True, exist_ok=True)
    files = [bundles / f'gseek-{arch}.flatpak' for arch in ('x86_64', 'aarch64')]
    if any(not path.is_file() or path.is_symlink() for path in files):
        raise ValueError('Both verified architecture bundles are required')
    with tarfile.open(output / 'gseek-repository-inputs.tar', 'w') as archive:
        for path in files:
            archive.add(path, arcname=path.name, recursive=False)
    key = base64.b64encode((Path(__file__).resolve().parents[1] / 'build-aux/fariszr.gpg').read_bytes()).decode('ascii')
    (output / 'gseek.flatpakref').write_text(f'''[Flatpak Ref]
Name=com.fariszr.gseek
Branch=stable
Title=GSeek
Url=https://flatpak.fariszr.com/repo
SuggestRemoteName=fariszr
IsRuntime=false
RuntimeRepo=https://flathub.org/repo/flathub.flatpakrepo
GPGKey={key}
''')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bundles', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    assemble(args.bundles, args.output)
