#!/usr/bin/env python3
"""Bundle the user-space GPU libraries needed by one inference helper.

The host supplies its kernel driver and Vulkan ICD. CUDA/ROCm SDKs are not
required on the user's machine. Only SDK redistributables, not driver stubs,
are collected; each bundle carries its SDK licenses.
"""
import argparse
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile

parser = argparse.ArgumentParser()
parser.add_argument("backend", choices=["cuda", "rocm"])
parser.add_argument("arch", choices=["x86_64", "aarch64"])
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
bundle = root / "dist/runtime" / args.backend
sdk = Path("/usr/local/cuda") if args.backend == "cuda" else Path("/opt/rocm")
sdk = sdk.resolve()
pending = [path for path in bundle.iterdir() if path.is_file()]
seen = set()
while pending:
    path = pending.pop()
    if path in seen:
        continue
    seen.add(path)
    result = subprocess.run(["ldd", str(path)], text=True, capture_output=True, check=False)
    for line in result.stdout.splitlines():
        match = re.search(r"^\s*(\S+) => (/\S+)", line)
        if not match:
            continue
        name, location = match.groups()
        source = Path(location).resolve()
        if not source.is_relative_to(sdk) or "/stubs/" in str(source):
            continue
        destination = bundle / name
        if not destination.exists():
            shutil.copy2(source, destination)
            pending.append(destination)

if args.backend == "rocm":
    kernels = sdk / "lib/rocblas/library"
    if not kernels.is_dir():
        kernels = sdk / "lib64/rocblas/library"
    if kernels.is_dir():
        target = bundle / "rocblas/library"
        target.mkdir(parents=True, exist_ok=True)
        architectures = ["gfx1030", "gfx1100", "gfx1101", "gfx1102", "gfx1200", "gfx1201"]
        for path in kernels.iterdir():
            if path.is_file() and ("gfx" not in path.name or any(arch in path.name for arch in architectures)):
                shutil.copy2(path, target / path.name)

licenses = bundle / "licenses"
licenses.mkdir(exist_ok=True)
for folder in [sdk / "share/doc", sdk / "share/licenses", sdk / "doc"]:
    if folder.is_dir():
        for path in folder.rglob("*"):
            if path.is_file() and any(word in path.name.lower() for word in ["license", "eula", "copying", "notice"]):
                dest = licenses / path.relative_to(sdk)
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, dest)
for name in ["EULA.txt", "LICENSE", "LICENSE.txt"]:
    if (sdk / name).is_file():
        shutil.copy2(sdk / name, licenses / name)
for path in bundle.iterdir():
    if path.is_file() and (".so" in path.name or path.name == "gseek-inference"):
        subprocess.run(["patchelf", "--set-rpath", "$ORIGIN", str(path)], check=True)

archive = root / f"dist/gseek-runtime-{args.backend}-{args.arch}.tar.gz"
with tarfile.open(archive, "w:gz") as output:
    output.add(bundle, arcname=args.backend)
print(archive.name, archive.stat().st_size, hashlib.sha256(archive.read_bytes()).hexdigest())
