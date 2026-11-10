# AudioPush for Android

The same protocol as the Windows build, so a phone and a PC talk to each
other with no configuration: **phone as speaker** for your PC, or **phone as
source** playing through your PC's speakers.

Minimum Android 8.0. Sending the phone's own audio additionally needs
Android 10 — see the limitation below, it is a real one.

---

## Build

```powershell
cd android
.\gradlew.bat assembleRelease
```

Output: `app/build/outputs/apk/release/app-release.apk` (~2.6 MB).

The release build is debug-signed on purpose, so `assembleRelease` gives you
an installable APK without a keystore. Swap in your own signing config before
publishing it anywhere.

Install over USB:

```powershell
adb install -r app\build\outputs\apk\release\app-release.apk
```

On Xiaomi/MIUI this fails with `INSTALL_FAILED_USER_RESTRICTED` until you turn
on **Install via USB** in Developer options.

---

## Receive — phone plays your PC's audio

1. Phone: open AudioPush, leave it on **Receive**, press **START**.
2. PC: run AudioPush, choose **SEND**, and the phone appears in the list by
   name. Click it, press START.

The phone announces itself on the discovery port exactly like the Windows
receiver, so there is nothing to type. Its addresses are shown on screen as a
fallback.

## Send — PC plays the phone's audio

1. PC: run AudioPush, choose **RECEIVE**, press START.
2. Phone: switch to **Send**, pick the PC from the list, press START, and
   grant the capture prompt.

### The limitation you need to know about

Android has no equivalent of WASAPI loopback. The nearest thing,
`AudioPlaybackCapture`, lets **each app decide whether it may be captured**.
An app that opts out is skipped silently by the platform — you get silence
from it, with no error, and no app can work around that. DRM-protected video
and several music services opt out.

What does work: most games, podcast and local-media players, browsers in many
cases, and anything that has not deliberately opted out. Test with the app
you actually care about before assuming.

---

## USB cable mode — lowest latency

WiFi is the variable in this system, not the code. Removing it helps more
than any tuning:

1. Connect the phone to the PC by USB.
2. On the phone: **Settings → Connection & sharing → USB tethering** (wording
   varies by vendor).
3. Windows gets a new network connection; the phone gets an address on it.
4. Run AudioPush on both ends as usual. The phone's wired address is listed
   **first** on the receive screen, labelled *USB / wired*.
5. Use the **Low** preset. Over USB there is nothing to buffer against.

Tethering is used rather than `adb forward` because adb only forwards TCP,
and this protocol is deliberately UDP — a retransmitted audio packet is a
late audio packet, which is worse than a missing one.

If the phone is also on WiFi, make sure the PC connects to the *wired*
address, not the WiFi one. The list on the phone shows which is which.

---

## Why it stays smooth

Everything from the Windows build is here:

- **No allocation in the audio path.** Pre-allocated ring and block buffers,
  so nothing the player thread does can trigger a garbage collection — on
  Android a GC pause in the audio thread *is* a dropout.
- **Urgent audio thread priority** and `PERFORMANCE_MODE_LOW_LATENCY`, which
  asks the platform for the fast mixer path.
- **Adaptive jitter buffer**, sized from the real AudioTrack buffer, then
  tightened while the link behaves and widened instantly on a dropout.
- **Clock drift correction**: the phone and the PC run off independent
  crystals, so the player writes one frame fewer or one frame more (20 µs)
  to hold the buffer centred indefinitely.
- **Dropout concealment** by fading the last frame rather than cutting to
  silence.
- **Send pacing**, so a capture buffer goes out spread evenly instead of as a
  burst.

Two things exist only on Android, and both matter more than any of the above:

- **`WIFI_MODE_FULL_LOW_LATENCY` WiFi lock.** Without it the radio sleeps
  between beacons and delivers packets in late bursts. This is the single
  biggest cause of hiccups on an otherwise healthy link.
- **Multicast lock.** Without it the WiFi chip filters broadcast traffic and
  discovery never arrives at all.

Streaming runs in a foreground service, so it survives the screen going off.

---

## Troubleshooting

**The PC does not see the phone.** Check the phone is **started**, not just
open. Then check both are on the same subnet — guest WiFi and AP isolation
block device-to-device traffic entirely. Then press **Scan** on the PC, which
uses unicast and gets through some access points that drop broadcast.

**Dropouts on WiFi.** Move to Balanced, then Stable. If `lost` is climbing it
is the radio; if `dropouts` climbs with no `lost`, the buffer is too shallow
for the link. Or use USB tethering and stop fighting it.

**Sending gives silence.** The app you are playing from has opted out of
audio capture. Nothing to fix; try a different app to confirm.

**Nothing after the screen turns off.** Check the AudioPush notification is
present. Some vendors (Xiaomi, Huawei, Samsung) kill foreground services
anyway — set AudioPush to *No restrictions* in the battery settings.
