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

---

## "The level meter moves but there is no sound"

Check the **Output:** line on the status card. It shows where Android is
actually sending playback, which separates three faults that look identical:

- **`Phone speaker` / `Bluetooth` with a sensible media volume** — routing is
  fine, so the problem is elsewhere.
- **`type 25`, or anything virtual** — media is being routed to
  `REMOTE_SUBMIX`, the sink used by screen recording, casting and playback
  capture. Audio written there is healthy and completely inaudible. The app
  now pins playback to a real output to defend against this, but if you see
  it, something on the phone is holding a capture session: stop any screen
  recorder, Cast/Smart View session, or audio-sharing feature.
- **A real device but `media volume 0%`** — turn the volume up.

This is also logged, so `adb logcat -s AudioPush` shows the routed device and
every available output at the moment the track opened.

---

## Keeping it playing with the screen off

A foreground service is not enough on its own. Android's Doze defers network
traffic for apps it considers idle, and vendor battery managers freeze them
outright — which is why a stream goes silent a minute after you lock the
phone. AudioPush does three things about it:

1. **Holds a partial wake lock** while streaming, so the receiving and mixing
   threads keep running with the screen off. Released the moment you stop.
2. **Holds a `WIFI_MODE_FULL_LOW_LATENCY` WiFi lock**, so the radio does not
   park between beacons.
3. **Asks you to allow background running.** The card at the top of the
   screen stays there until you do; tapping it opens the system dialog. Only
   you can grant this, so the app cannot do it quietly.

### Xiaomi, HyperOS and MIUI

These are more aggressive than stock Android and need three more settings,
none of which an app can set for you:

- **Settings → Apps → AudioPush → Battery saver → No restrictions**
- **Settings → Apps → AudioPush → Autostart → on**
- Open Recents, swipe down on the AudioPush card (or long-press it) and
  **lock** it, so clearing recents does not kill it.

Samsung (*Never sleeping apps*), Huawei (*App launch → Manage manually*) and
OnePlus (*Battery optimisation → Don't optimise*) have equivalents.

If audio still stops, check whether the AudioPush notification is still in
the shade. If it has gone, the system killed the service and the settings
above are the fix. If it is still there but silent, it is the network being
deferred — which is what the battery exemption addresses.
