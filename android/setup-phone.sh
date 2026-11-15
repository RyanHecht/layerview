#!/usr/bin/env bash
# One-time phone setup. Prerequisites on the phone: factory reset, NO Google
# account added, USB debugging enabled and this computer authorized.
set -euo pipefail
APK="${1:?usage: setup-phone.sh path/to/layerview-kiosk.apk}"
PKG="io.github.ryanhecht.layerview.kiosk"
adb install -r -t "$APK"                      # -t: test-only app (device owner is removable via adb)
adb shell dpm set-device-owner "$PKG/.AdminReceiver"
adb reverse tcp:8740 tcp:8740
adb shell am start -n "$PKG/.MainActivity"
echo "Done. Kiosk is device owner; the phone shows http://localhost:8740 (this computer's layerview)."
echo "Maintenance: adb shell am task lock stop   (leave kiosk mode until next launch)"
echo "Remove:      adb shell dpm remove-active-admin $PKG/.AdminReceiver && adb uninstall $PKG"
