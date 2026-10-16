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
  # Reuse the host ABI. Fedora's OpenSSL 4-only installations need compatibility libraries.
  ssl_dirs = ["/usr/lib64", "/lib64", "/usr/lib/#{{arch}}-linux-gnu",
              "/lib/#{{arch}}-linux-gnu", "/usr/lib", "/lib"]
  system_ssl3 = ssl_dirs.any? do |dir|
    File.file?(File.join(dir, "libssl.so.3")) && File.file?(File.join(dir, "libcrypto.so.3"))
  end
  depends_on formula: "openssl@3" unless system_ssl3
  runtime_libraries = system_ssl3 ? "" : "{{{{HOMEBREW_PREFIX}}}}/opt/openssl@3/lib"

  artifact "gseek", target: "~/.local/share/gseek-app"
  binary "#{{Dir.home}}/.local/share/gseek-app/bin/gseek", target: "gseek"

  # Homebrew replaces HOME and preserves desktop variables as HOMEBREW_*.
  # Give the registration command the actual user's XDG and session paths.
  integration_env = {{}}
  {{"XDG_DATA_HOME" => ".local/share", "XDG_CONFIG_HOME" => ".config",
    "XDG_CACHE_HOME" => ".cache"}}.each do |name, fallback|
    value = ENV["HOMEBREW_#{{name}}"] || ENV[name]
    integration_env[name] = if value && Pathname.new(value).absolute?
      value
    else
      File.join(Dir.home, fallback)
    end
  end
  %w[DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR].each do |name|
    value = ENV["HOMEBREW_#{{name}}"] || ENV[name]
    integration_env[name] = value if value && !value.empty?
  end
  user_paths = [File.join(Dir.home, ".local/share"),
                *integration_env.values_at("XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME")].uniq

  app_launcher = File.join(Dir.home, ".local/share/gseek-app/bin/gseek")

  postflight_steps do
    run "/usr/bin/install",
        args: ["-D", "-m", "644", "{{{{staged_path}}}}/gseek-search-provider.ini",
               "/usr/local/share/gnome-shell/search-providers/io.github.FarisZR.GSeek.ini"],
        sudo: true
    write_file "~/.local/share/gseek-app/homebrew-prefix", "{{{{HOMEBREW_PREFIX}}}}\\n"
    write_file "~/.local/share/gseek-app/runtime-library-path", "#{{runtime_libraries}}\\n"
    run app_launcher,
        args: ["integrate"],
        env: integration_env,
        writable_paths: user_paths
  end

  uninstall_preflight_steps do
    run app_launcher,
        args: ["unintegrate"],
        env: integration_env,
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
