#!/bin/sh
# Put the right v4l2loopback driver behind the "MarkCam" camera and (re)load it. Run as root:
# by the package's postinst, and by MarkCam's "Repair camera" button (through pkexec).
#
#   camera-driver.sh [--install-packages]
#
# Kernels from 7.0 on ship their own v4l2loopback (0.15). Ubuntu's v4l2loopback-dkms (0.12.7)
# also builds for them, but there it breaks once the camera is closed and reopened ("can't set
# the picture format"). /etc/depmod.d/markcam-v4l2loopback.conf makes the kernel's own driver
# win whenever it is installed; --install-packages installs it for the running kernel (plus the
# package that keeps it installed for future kernel updates). apt can't run inside postinst,
# so only the app passes it.
set -u
K="$(uname -r)"

if [ "${1:-}" = "--install-packages" ] && command -v apt-get >/dev/null 2>&1; then
    pkgs=""
    if apt-cache show "linux-main-modules-v4l2loopback-$K" >/dev/null 2>&1; then
        pkgs="linux-main-modules-v4l2loopback-$K"
        # linux-image-generic-hwe-24.04 → linux-modules-v4l2loopback-generic-hwe-24.04, etc.
        for meta in $(dpkg-query -W -f '${db:Status-Abbrev} ${Package}\n' 'linux-image-*' 2>/dev/null \
                      | awk '$1 == "ii" { print $2 }' | grep -v -- '-[0-9]\+\.[0-9]\+\.[0-9]\+-'); do
            m="linux-modules-v4l2loopback-${meta#linux-image-}"
            apt-cache show "$m" >/dev/null 2>&1 && pkgs="$pkgs $m"
        done
    fi
    if [ -n "$pkgs" ]; then
        # shellcheck disable=SC2086
        DEBIAN_FRONTEND=noninteractive apt-get install -y $pkgs >/dev/null 2>&1 ||
            echo "MarkCam: could not install $pkgs (is another installer running?)" >&2
    fi
fi

depmod -a "$K"

# Reload when the loaded driver isn't the one modprobe would pick now, or when asked to
# repair. Not possible while an app still has the camera open.
want="$(modinfo -F srcversion v4l2loopback 2>/dev/null)"
have="$(cat /sys/module/v4l2loopback/srcversion 2>/dev/null)"
if [ -n "$have" ]; then
    if [ "$have" != "$want" ] || [ "${1:-}" = "--install-packages" ] || ! grep -qsx MarkCam /sys/class/video4linux/*/name; then
        if ! modprobe -r v4l2loopback 2>/dev/null; then
            echo "MarkCam: the camera driver is in use (close ClassIn / Zoom / the browser and try again, or restart the computer)." >&2
            exit 2
        fi
    fi
fi
if ! lsmod | grep -q '^v4l2loopback '; then
    modprobe v4l2loopback || {
        echo "MarkCam: the virtual camera driver could not be loaded now. Restart the computer (with Secure Boot on, enroll the key you were asked to set during the install)." >&2
        exit 1
    }
fi
exit 0
