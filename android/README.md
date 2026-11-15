# Phone as a permanent layer display

Coming next: a script and udev rule that, when the phone is plugged in, forward
its `localhost:8740` to the computer (`adb reverse`), keep the screen on, and
open the display full screen. No Wi-Fi, nothing exposed on the network.
