#!/usr/bin/env python3
"""Select real-model checks from changes to inference code and its dependencies."""
import json
import os
from pathlib import Path
import subprocess
import tomllib

INFERENCE_PATHS = {
    "src/inference.rs", "src/managed_runtime.rs", "src/model.rs",
    "src/local_http.rs", "src/cancellation.rs", "src/extract.rs",
    "data/runtimes.json", "data/reference-embedding.json",
    "tests/real_inference.rs", "scripts/ci-deps.sh", "build-aux/com.fariszr.gseek.json",
}
DESKTOP_DEPENDENCIES = {"gtk", "adw", "gio", "glib", "clap", "rusqlite", "unicode-normalization"}


def requires_search(paths, inference):
    return inference or any(path.startswith(("src/", "build-aux/", "data/com.fariszr.gseek")) or path in {
        "Cargo.toml", "Cargo.lock", "tests/e2e.py", "tests/run-e2e.sh", "tests/shell.py",
        "tests/install-fixture.py", "tests/flatpak-fixture.py", "tests/flatpak-data.py",
        ".github/workflows/ci.yml"
    } for path in paths)


def dependencies(manifest):
    """Include shared/runtime crates; desktop-only updates do not load models."""
    return {name: value for name, value in tomllib.loads(manifest).get("dependencies", {}).items()
            if name not in DESKTOP_DEPENDENCIES}


def lock_signature(lock, roots):
    packages = tomllib.loads(lock).get("package", [])
    selected = set(roots)
    pending = list(roots)
    by_name = {}
    for package in packages:
        by_name.setdefault(package["name"], []).append(package)
    while pending:
        for package in by_name.get(pending.pop(), []):
            for dependency in package.get("dependencies", []):
                name = dependency.split()[0]
                if name not in selected:
                    selected.add(name)
                    pending.append(name)
    return sorted(json.dumps(package, sort_keys=True) for package in packages if package["name"] in selected)


def requires_inference(paths, before_manifest, after_manifest, before_lock, after_lock):
    if INFERENCE_PATHS.intersection(paths):
        return True
    before = dependencies(before_manifest)
    after = dependencies(after_manifest)
    return before != after or lock_signature(before_lock, before) != lock_signature(after_lock, after)


def git(*arguments):
    return subprocess.check_output(["git", *arguments], text=True)


def main():
    event_name = os.environ["GITHUB_EVENT_NAME"]
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    run = True  # Manual runs intentionally exercise the full suite.
    search = True
    if event_name in {"pull_request", "push"}:
        base = event["pull_request"]["base"]["sha"] if event_name == "pull_request" else event["before"]
        head = event["pull_request"]["head"]["sha"] if event_name == "pull_request" else event["after"]
        if set(base) != {"0"}:
            paths = git("diff", "--name-only", "--no-renames", base, head, "--").splitlines()
            def read(revision, name):
                return git("show", f"{revision}:{name}")
            run = requires_inference(paths, read(base, "Cargo.toml"), read(head, "Cargo.toml"),
                                     read(base, "Cargo.lock"), read(head, "Cargo.lock"))
            search = requires_search(paths, run)
    value = str(run).lower()
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"inference={value}\n")
        output.write(f"search={str(search).lower()}\n")
    print(f"Real-model inference checks: {value}")
    print(f"CPU search integration checks: {str(search).lower()}")


if __name__ == "__main__":
    main()
