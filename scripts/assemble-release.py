#!/usr/bin/env python3
"""Publish native app packages. The app consumes upstream inference distributions."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil

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
checksums = {}
for architecture in ["aarch64", "x86_64"]:
    name = f"gseek-linux-{architecture}.tar.gz"
    source = artifact(name)
    target = args.output / name
    shutil.copyfile(source, target)
    checksums[architecture] = digest(target)
(args.output / "gseek.rb").write_text(cask.render(args.version, f"{base}/gseek-linux-#{{arch}}.tar.gz", checksums["aarch64"], checksums["x86_64"]))
(args.output / "SHA256SUMS").write_text("".join(f"{digest(path)}  {path.name}\n" for path in sorted(args.output.iterdir()) if path.is_file() and path.name != "SHA256SUMS"))
print(json.dumps({"version": args.version, "artifacts": sorted(path.name for path in args.output.iterdir())}, indent=2))
