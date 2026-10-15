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

  user_paths = [
    File.join(Dir.home, ".local/share"),
    File.join(Dir.home, ".config"),
    File.join(Dir.home, ".cache"),
    *ENV.values_at("XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"),
  ].compact.select {{ |path| Pathname.new(path).absolute? }}

  postflight_steps do
    run "/usr/bin/install",
        args: ["-D", "-m", "644", "{{{{staged_path}}}}/gseek-search-provider.ini",
               "/usr/local/share/gnome-shell/search-providers/io.github.FarisZR.GSeek.ini"],
        sudo: true,
        writable_paths: ["/usr/local/share/gnome-shell/search-providers"]
    write_file "~/.local/share/gseek-app/homebrew-prefix", "{{{{HOMEBREW_PREFIX}}}}\\n"
    run "~/.local/share/gseek-app/bin/gseek",
        args: ["integrate"],
        writable_paths: user_paths
  end

  uninstall_preflight_steps do
    run "~/.local/share/gseek-app/bin/gseek",
        args: ["unintegrate"],
        writable_paths: user_paths
  end

  uninstall_postflight_steps do
    remove "/usr/local/share/gnome-shell/search-providers/io.github.FarisZR.GSeek.ini", sudo: true
  end

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
