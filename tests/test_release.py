"""Exercise the release boundary with small real archives and immutable upstream metadata."""
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
ASSEMBLER = ROOT / "scripts/assemble-release.py"


class ReleaseAssembly(unittest.TestCase):
    def test_native_packages_are_published_unchanged_with_verified_checksums(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for architecture in ["x86_64", "aarch64"]:
                with tarfile.open(root / f"gseek-linux-{architecture}.tar.gz", "w:gz") as archive:
                    data = b"native launcher"
                    entry = tarfile.TarInfo("gseek/bin/gseek")
                    entry.size = len(data)
                    archive.addfile(entry, io.BytesIO(data))
            output = root / "release"
            subprocess.run(["python3", str(ASSEMBLER), "0.1.0", str(root), str(output)], check=True, capture_output=True)
            self.assertEqual({p.name for p in output.iterdir()}, {
                "gseek-linux-x86_64.tar.gz", "gseek-linux-aarch64.tar.gz", "gseek.rb", "SHA256SUMS"})
            for architecture in ["x86_64", "aarch64"]:
                name = f"gseek-linux-{architecture}.tar.gz"
                self.assertEqual((root / name).read_bytes(), (output / name).read_bytes())
            for line in (output / "SHA256SUMS").read_text().splitlines():
                checksum, name = line.split("  ")
                self.assertEqual(hashlib.sha256((output / name).read_bytes()).hexdigest(), checksum)
            cask = (output / "gseek.rb").read_text()
            self.assertIn('version "0.1.0"', cask)
            self.assertIn('args: ["unintegrate"]', cask)

    def test_incomplete_release_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = subprocess.run(["python3", str(ASSEMBLER), "0.1.0", str(root), str(root / "release")], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Expected exactly one", result.stderr)

    def test_upstream_runtime_contract_covers_all_backends_and_architectures(self):
        manifest = json.loads((ROOT / "data/runtimes.json").read_text())
        self.assertEqual(manifest["version"], 1)
        self.assertEqual(set(manifest["assets"]), {"cpu", "vulkan", "cuda"})
        for backend, architectures in manifest["assets"].items():
            self.assertEqual(set(architectures), {"x86_64", "aarch64"})
            for assets in architectures.values():
                self.assertEqual(len(assets), 2 if backend == "cuda" else 1)
                for asset in assets:
                    self.assertTrue(asset["url"].startswith(f'https://github.com/ggml-org/llama.cpp/releases/download/{manifest["release"]}/'))
                    self.assertRegex(asset["sha256"], r"^[0-9a-f]{64}$")
                    self.assertGreater(asset["bytes"], 0)
                if backend == "cuda":
                    self.assertIn("/cudart-", assets[1]["url"])


if __name__ == "__main__":
    unittest.main()
