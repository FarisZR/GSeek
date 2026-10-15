#!/usr/bin/env python3
"""Assemble immutable core packages and their matching managed GPU manifest."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import tarfile
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("version")
parser.add_argument("artifacts", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
spec = importlib.util.spec_from_file_location("cask", Path(__file__).with_name("render-cask.py"))
cask = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cask)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def artifact(name):
    matches = list(args.artifacts.rglob(name))
    if len(matches) != 1:
        raise ValueError(f"Expected exactly one {name}; found {len(matches)}")
    return matches[0]


args.output.mkdir(parents=True, exist_ok=True)
base = f"https://github.com/FarisZR/GSeek/releases/download/v{args.version}"
manifest = {"version": 1, "assets": {}}
for backend, architecture in [("cuda", "x86_64"), ("cuda", "aarch64"), ("rocm", "x86_64")]:
    name = f"gseek-runtime-{backend}-{architecture}.tar.gz"
    source = artifact(name)
    target = args.output / name
    shutil.copyfile(source, target)
    manifest["assets"].setdefault(backend, {})[architecture] = {
        "url": f"{base}/{name}", "sha256": digest(target), "bytes": target.stat().st_size,
    }
manifest_text = json.dumps(manifest, indent=2) + "\n"
(args.output / "runtime-manifest.json").write_text(manifest_text)
checksums = {}
for architecture in ["aarch64", "x86_64"]:
    name = f"gseek-linux-{architecture}.tar.gz"
    source = artifact(name)
    with tempfile.TemporaryDirectory(prefix="gseek-release-") as directory:
        staging = Path(directory)
        with tarfile.open(source) as archive:
            archive.extractall(staging, filter="data")
        (staging / "gseek/runtime-manifest.json").write_text(manifest_text)
        target = args.output / name
        with tarfile.open(target, "w:gz") as archive:
            for item in sorted(staging.iterdir()):
                archive.add(item, arcname=item.name)
    checksums[architecture] = digest(target)
(args.output / "gseek.rb").write_text(cask.render(args.version, f"{base}/gseek-linux-#{{arch}}.tar.gz", checksums["aarch64"], checksums["x86_64"]))
(args.output / "SHA256SUMS").write_text("".join(f"{digest(path)}  {path.name}\n" for path in sorted(args.output.iterdir()) if path.is_file() and path.name != "SHA256SUMS"))
print(json.dumps({"version": args.version, "artifacts": sorted(path.name for path in args.output.iterdir())}, indent=2))
