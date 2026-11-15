# Phone as a permanent layer display

A spare Android phone, plugged into the computer by USB, shows layerview full
screen. No Wi-Fi: the phone reaches the computer's `localhost:8740` through
`adb reverse` over the cable, so the key stream never touches your network.

## Pieces

- `kiosk/`: a tiny app: one full-screen WebView of `http://localhost:8740`.
  As **device owner** (Android's dedicated-device mode) it pins itself, disables
  the lock screen and notification shade, stays awake while plugged in, and is
  the home app, so it comes back after reboots. Built test-only so `adb` alone
  can remove it.
- `setup-phone.sh`: one-time phone setup over adb.
- `layerview-phone-link`: keeps `adb reverse` in place whenever the phone
  (re)connects. Run it as a user service.
- `systemd/`: user units for `layerview` and the phone link.

## Setup

1. Factory reset the phone. During setup **don't add a Google account**
   (device owner requires none), skip the screen lock, enable USB debugging,
   authorize this computer.
2. Build the app: `cd kiosk && gradle assembleDebug` (JDK 17+, Android SDK 36).
3. `./setup-phone.sh kiosk/app/build/outputs/apk/debug/app-debug.apk`
4. On the computer:
   ```sh
   install -m755 layerview-phone-link ~/.local/bin/
   cp systemd/*.service ~/.config/systemd/user/
   systemctl --user enable --now layerview.service layerview-phone-link.service
   ```

## Battery

A phone left on a charger forever is hard on its battery. If the phone offers a
charge limit (Pixels: Settings → Battery → Charging optimization → Limit to 80%,
Android 15+), turn it on. Kiosk mode blocks the Settings app, so for maintenance:
`adb shell am task lock stop`, then `adb shell am start -a android.settings.SETTINGS`.
