#!/usr/bin/env python3
"""Inventory the exact locked Rust sources and their declared licenses."""
import json
import sys

metadata = json.load(sys.stdin)
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    print(f'{package["name"]} {package["version"]}: {package.get("license") or "See upstream license file"}')
    print(f'  {package.get("source") or package.get("repository") or "GSeek source tree"}')
