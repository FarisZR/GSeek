#!/usr/bin/env bash
set -euo pipefail
if command -v apt-get >/dev/null; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  sparql_dev=libtinysparql-3.0-dev
  sparql_gir=gir1.2-tinysparql-3.0
  apt-cache show "$sparql_dev" >/dev/null 2>&1 || sparql_dev=libtracker-sparql-3.0-dev
  apt-cache show "$sparql_gir" >/dev/null 2>&1 || sparql_gir=gir1.2-tracker-3.0
  apt-get install -y --no-install-recommends build-essential pkg-config curl ca-certificates git \
    libgtk-4-dev libadwaita-1-dev "$sparql_dev" "$sparql_gir" python3-gi python3-gi-cairo \
    dbus-daemon dbus-x11 xvfb xauth cmake ninja-build libvulkan-dev glslc spirv-headers \
    mesa-vulkan-drivers vulkan-tools poppler-utils ffmpeg libarchive-tools gnome-shell \
    gsettings-desktop-schemas adwaita-icon-theme fonts-dejavu-core fonts-noto-core
elif command -v dnf >/dev/null; then
  dnf install -y gcc gcc-c++ pkgconf-pkg-config curl ca-certificates git \
    gtk4-devel libadwaita-devel tinysparql-devel python3-gobject python3-cairo \
    dbus-daemon dbus-x11 xorg-x11-server-Xvfb xorg-x11-xauth cmake ninja-build \
    vulkan-headers vulkan-loader-devel glslc spirv-headers mesa-vulkan-drivers vulkan-tools \
    poppler-utils ffmpeg-free libarchive gnome-shell gsettings-desktop-schemas \
    adwaita-icon-theme google-noto-sans-fonts google-noto-sans-arabic-fonts
else
  echo 'Unsupported CI image' >&2; exit 1
fi
