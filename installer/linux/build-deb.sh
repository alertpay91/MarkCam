#!/usr/bin/env bash
# Build dist/markcam_<version>_amd64.deb on Linux (Ubuntu 24.04 / Linux Mint 22 or WSL).
#
#   installer/linux/build-deb.sh <version> <stage dir> <deps dir>
#
# <stage dir>: what installer/build-deb.ps1 gathered on Windows — models/, effects/,
#   backgrounds/, licenses/, LICENSE.txt, THIRD-PARTY-NOTICES.txt, source/.
# <deps dir>: onnxruntime-linux-x64-<ver>/ and yt-dlp (yt-dlp_linux).
#
# The package installs MarkCam in /opt/markcam (same layout as on Windows: the binary with
# models/, effects/, backgrounds/ next to it), /usr/bin/markcam, a menu entry, and the
# v4l2loopback options that make the "MarkCam" camera appear at boot.
set -euo pipefail
VERSION="$1"
STAGE="$(realpath "$2")"
DEPS="$(realpath "$3")"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HERE="$ROOT/installer/linux"
ORT="$(ls -d "$DEPS"/onnxruntime-linux-x64-* | sort -V | tail -1)"

cd "$ROOT"
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
export MARKCAM_PUBLIC=1
nice -n 10 cargo build --release -p ovc-app

PKG="$(mktemp -d)/markcam_${VERSION}_amd64"
OPT="$PKG/opt/markcam"
mkdir -p "$OPT/tools" "$PKG/DEBIAN" "$PKG/usr/bin" "$PKG/usr/share/applications" \
         "$PKG/usr/share/icons/hicolor/256x256/apps" "$PKG/usr/share/doc/markcam" \
         "$PKG/etc/modprobe.d" "$PKG/etc/modules-load.d"

install -m 755 target/release/markcam "$OPT/markcam"
strip "$OPT/markcam"
# ONNX Runtime (MIT), loaded at run time like onnxruntime.dll on Windows.
cp -a "$ORT"/lib/libonnxruntime.so* "$OPT/"
install -m 755 "$DEPS/yt-dlp" "$OPT/tools/yt-dlp"
cp -r "$STAGE/models" "$STAGE/effects" "$STAGE/backgrounds" "$STAGE/licenses" "$OPT/"
cp "$ORT/LICENSE" "$OPT/licenses/onnxruntime-LICENSE.txt"
cp "$ORT/ThirdPartyNotices.txt" "$OPT/licenses/onnxruntime-ThirdPartyNotices.txt"
rm -f "$OPT/licenses/DirectML-LICENSE.txt"
cp "$STAGE/LICENSE.txt" "$STAGE/THIRD-PARTY-NOTICES.txt" "$OPT/"
cp "$HERE/README-LINUX.txt" "$OPT/README.txt"
[ -d "$STAGE/source" ] && cp -r "$STAGE/source" "$OPT/"
ln -s /opt/markcam/markcam "$PKG/usr/bin/markcam"
install -m 644 "$HERE/markcam.desktop" "$PKG/usr/share/applications/markcam.desktop"
install -m 644 crates/ovc-app/assets/markcam.png "$PKG/usr/share/icons/hicolor/256x256/apps/markcam.png"
install -m 644 "$HERE/markcam-v4l2loopback.conf" "$PKG/etc/modprobe.d/markcam-v4l2loopback.conf"
printf 'v4l2loopback\n' > "$PKG/etc/modules-load.d/markcam.conf"
chmod 644 "$PKG/etc/modules-load.d/markcam.conf"
cat > "$PKG/usr/share/doc/markcam/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: MarkCam
Source: https://github.com/alertpay91/MarkCam

Files: *
Copyright: Marko Stevanović
License: GPL-3.0-or-later
 See /opt/markcam/LICENSE.txt; third-party parts: /opt/markcam/THIRD-PARTY-NOTICES.txt
 and /opt/markcam/licenses/.
EOF
for s in postinst postrm; do install -m 755 "$HERE/$s" "$PKG/DEBIAN/$s"; done

# Shared-library dependencies of the binary, worked out by dpkg-shlibdeps.
mkdir -p "$PKG/debian" && printf 'Source: markcam\n' > "$PKG/debian/control"
SHLIBS="$(cd "$PKG" && dpkg-shlibdeps -O -l"$OPT" -e"opt/markcam/markcam" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')"
rm -rf "$PKG/debian"
SIZE="$(du -sk --exclude=DEBIAN "$PKG" | cut -f1)"
cat > "$PKG/DEBIAN/control" <<EOF
Package: markcam
Version: $VERSION
Section: video
Priority: optional
Architecture: amd64
Maintainer: Marko Stevanović <alertpay91@users.noreply.github.com>
Installed-Size: $SIZE
Depends: ${SHLIBS:+$SHLIBS, }libxkbcommon0, libxkbcommon-x11-0, libx11-6, libx11-xcb1, libxcursor1, libxrandr2, libxi6, libgl1, libegl1, libvulkan1, libwayland-client0, libwayland-cursor0, libwayland-egl1, fonts-dejavu-core, v4l2loopback-dkms, v4l2loopback-utils, ffmpeg, pulseaudio-utils, slop, libglib2.0-bin, xdg-user-dirs, xdg-utils, pkexec | policykit-1
Recommends: libgl1-mesa-dri, mesa-vulkan-drivers, fonts-noto-cjk
Homepage: https://github.com/alertpay91/MarkCam
Description: Virtual camera with live video effects for online lessons
 MarkCam turns your webcam into a "MarkCam" camera for Zoom, Google Meet,
 Teams, ClassIn (web), OBS and browsers, with face-tracking effects, virtual
 backgrounds, name banners, classroom speech bubbles, videos and YouTube
 links as the picture, and a "MarkCam Microphone" that carries your voice
 plus the sound of what you play.
EOF
mkdir -p "$ROOT/dist"
OUT="$ROOT/dist/markcam_${VERSION}_amd64.deb"
dpkg-deb --root-owner-group -Zxz --build "$PKG" "$OUT"
rm -rf "$(dirname "$PKG")"
ls -la "$OUT"
