"""Exercise the release boundary with small real archives, without a GPU SDK."""
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ASSEMBLER = Path(__file__).resolve().parents[1] / "scripts/assemble-release.py"


class ReleaseAssembly(unittest.TestCase):
    def test_packages_contain_the_exact_gpu_contract_and_verified_checksums(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for architecture in ["x86_64", "aarch64"]:
                with tarfile.open(root / f"gseek-linux-{architecture}.tar.gz", "w:gz") as archive:
                    data = b"native launcher"
                    entry = tarfile.TarInfo("gseek/bin/gseek")
                    entry.size = len(data)
                    archive.addfile(entry, io.BytesIO(data))
                    provider = tarfile.TarInfo("gseek-search-provider.ini")
                    provider.size = 8
                    archive.addfile(provider, io.BytesIO(b"provider"))
            for backend, architecture in [("cuda", "x86_64"), ("cuda", "aarch64"), ("rocm", "x86_64")]:
                (root / f"gseek-runtime-{backend}-{architecture}.tar.gz").write_bytes(f"{backend}-{architecture}".encode())
            output = root / "release"
            subprocess.run(["python3", str(ASSEMBLER), "0.1.0", str(root), str(output)], check=True, capture_output=True)
            manifest = json.loads((output / "runtime-manifest.json").read_text())
            self.assertEqual(manifest["version"], 1)
            self.assertEqual(set(manifest["assets"]), {"cuda", "rocm"})
            for backend, architectures in manifest["assets"].items():
                for architecture, asset in architectures.items():
                    data = (output / f"gseek-runtime-{backend}-{architecture}.tar.gz").read_bytes()
                    self.assertEqual(asset["sha256"], hashlib.sha256(data).hexdigest())
                    self.assertEqual(asset["bytes"], len(data))
                    self.assertTrue(asset["url"].startswith("https://github.com/FarisZR/GSeek/releases/download/v0.1.0/"))
            for architecture in ["x86_64", "aarch64"]:
                with tarfile.open(output / f"gseek-linux-{architecture}.tar.gz") as archive:
                    self.assertEqual(json.load(archive.extractfile("gseek/runtime-manifest.json")), manifest)
                    self.assertEqual(archive.extractfile("gseek-search-provider.ini").read(), b"provider")
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


if __name__ == "__main__":
    unittest.main()
