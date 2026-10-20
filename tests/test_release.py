"""Exercise the release boundary with small real archives and immutable upstream metadata."""
import hashlib
import io
import json
import re
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]


class ReleaseAssembly(unittest.TestCase):
    def test_every_control_icon_is_shipped_in_the_gtk_resource(self):
        xml = ET.parse(ROOT / "data/gseek.gresource.xml")
        files = {node.text for node in xml.findall(".//file")}
        names = set(re.findall(r'"([a-z][a-z0-9-]*-symbolic)"', (ROOT / "src/ui.rs").read_text()))
        for name in names:
            path = f"icons/scalable/actions/{name}.svg"
            self.assertIn(path, files, f"Control icon {name} must not depend on the desktop theme")
            image = ET.parse(ROOT / "data" / path).getroot()
            self.assertEqual(image.tag, "{http://www.w3.org/2000/svg}svg")

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
