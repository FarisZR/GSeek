#!/usr/bin/env python3
"""Render the Linux cask used by release and lifecycle tests."""
import argparse
from pathlib import Path
import re


def render(version, url, arm_sha, intel_sha):
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?", version):
        raise ValueError("Invalid version")
    for checksum in [arm_sha, intel_sha]:
        if not re.fullmatch(r"[0-9a-f]{64}", checksum):
            raise ValueError("Invalid SHA-256")
    if '"' in url or "\n" in url or not url.startswith(("https://github.com/FarisZR/GSeek/", "file:///")):
        raise ValueError("Invalid package URL")
    return f'''cask "gseek" do
  arch arm: "aarch64", intel: "x86_64"

  version "{version}"
  sha256 arm64_linux:  "{arm_sha}",
         x86_64_linux: "{intel_sha}"

  url "{url}"
  name "GSeek"
  desc "Private multilingual semantic file search for GNOME"
  homepage "https://github.com/FarisZR/GSeek"

  depends_on :linux
  depends_on formula: %w[ffmpeg poppler libarchive]

  artifact "gseek", target: "~/.local/share/gseek-app"
  binary "#{{Dir.home}}/.local/share/gseek-app/bin/gseek", target: "gseek"

  postflight do
    system_command "/usr/bin/install",
                   args: ["-D", "-m", "644", (staged_path/"gseek-search-provider.ini").to_s,
                          "/usr/local/share/gnome-shell/search-providers/io.github.FarisZR.GSeek.ini"],
                   sudo: true,
                   must_succeed: true
    bundle = Pathname.new(Dir.home)/".local/share/gseek-app"
    (bundle/"homebrew-prefix").write("#{{HOMEBREW_PREFIX}}\\n")
    system_command bundle/"bin/gseek",
                   args: ["integrate", "--launcher", (bundle/"bin/gseek").to_s],
                   must_succeed: true
  end

  uninstall script: {{
    executable: "#{{Dir.home}}/.local/share/gseek-app/bin/gseek",
    args: ["unintegrate"],
    sudo: false,
  }},
  delete: "/usr/local/share/gnome-shell/search-providers/io.github.FarisZR.GSeek.ini"

  caveats <<~EOS
    Open GSeek from your application menu to finish setup. The model downloads
    once (865 MB). GNOME 51 is the supported desktop; your files stay local.
    GSeek can enable its provider and disable Files search during setup.
    The system search-provider registration requires administrator permission.
    Settings and the search index are preserved on upgrade and uninstall.
  EOS
end
'''


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    parser.add_argument("arm_sha")
    parser.add_argument("intel_sha")
    parser.add_argument("--url")
    args = parser.parse_args()
    url = args.url or f"https://github.com/FarisZR/GSeek/releases/download/v{args.version}/gseek-linux-#{{arch}}.tar.gz"
    print(render(args.version, url, args.arm_sha, args.intel_sha), end="")
