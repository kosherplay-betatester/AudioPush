# AudioPush

Play your workstation's sound through your laptop's speakers, over WiFi, with
a delay you stop noticing after about a minute.

One `.exe`. No installer, no service, no runtime. Run it on both PCs.

---

## Quick start

1. Build it (once): `.\build.ps1` — output is `target\release\audiopush.exe`.
2. Copy that one file to **both** PCs.
3. **Laptop (speakers):** run it → `RECEIVE on this PC` → `START`.
   Approve the Windows firewall prompt for **Private networks**.
4. **Workstation (no speakers):** run it → `SEND from this PC` → click the
   laptop in the receivers list → `START`.

Sound comes out of the laptop. The level meter on both ends shows audio
moving; if it moves on the sender but not the receiver, it is a firewall.

---

## Android

There is an Android build in [`android/`](android/) that speaks the same
protocol, so a phone and a PC find each other with nothing to configure:

- **Phone as a speaker** for the PC — works fully.
- **Phone as a source**, playing through the PC — works, with one platform
  caveat: Android lets each app refuse to have its audio captured, and several
  streaming apps do. Those arrive as silence.
- **USB cable mode** via USB tethering, which removes WiFi from the equation
  entirely and is the lowest-latency path available.

Build it with `cd android && .\gradlew.bat assembleRelease`. See
[android/README.md](android/README.md) for the details, the capture
limitation and the USB setup.

---

## Building

Requires [Rust](https://rustup.rs) (stable, MSVC toolchain). Then:

```powershell
.\build.ps1          # release build
.\build.ps1 -Test    # run the unit tests first
.\build.ps1 -Debug   # debug build, keeps a console window for logs
```

Or directly:

```powershell
cargo build --release
```

The result is a single statically-linked executable. Nothing else ships with it.

---

## Running the receiver (the laptop)

Choose `RECEIVE on this PC`.

- **Play through** — which speakers or headphones to use. "Default output
  (follow Windows)" tracks whatever you set in Windows, including changes made
  while streaming.
- **Tell the sender to connect to** — every IP address this machine has. You
  normally will not need these: the receiver announces itself by name and the
  sender lists it automatically. They are there for the case where broadcast
  traffic is blocked.
- `START` begins listening on UDP **45778** and announcing on UDP **45777**.

The receiver must be started *before* the sender can see it in the list.

## Running the sender (the workstation)

Choose `SEND from this PC`.

- **Capture from** — the output endpoint to tap. AudioPush uses WASAPI
  loopback, so it captures exactly what Windows plays to that endpoint,
  without changing anything about how your apps work.
- **Receivers on this network** — live list, updated once a second. Click one.
- **Scan network** — fallback if the list stays empty. It asks every address on
  your subnet whether AudioPush is listening there; any receiver answers at
  once. Machines not running AudioPush ignore it.
- **Or type an address** — last resort. `192.168.1.20`, or
  `192.168.1.20:45778` if you changed the port.
- `START` begins streaming.

## Latency presets

| Preset | Jitter margin | Typical end-to-end | Use when |
|---|---|---|---|
| **Low** | 8 ms | ~20 ms | Wired Ethernet, or strong 5 GHz WiFi |
| **Balanced** | 20 ms | ~55 ms | Default. Normal WiFi |
| **Stable** | 50 ms | ~145 ms | Crowded 2.4 GHz, weak signal, dropouts |

The jitter margin is buffering held *on top of* one device period. The buffer
target is computed from the real device buffer size at start-up, because WASAPI
drains a whole device buffer per callback — a target smaller than that quantum
underruns on every single callback no matter how good the network is. The
receiver screen shows the actual measured depth in milliseconds.

Low = less delay but more sensitive to WiFi hiccups. Change the preset while
stopped; it applies on the next `START`.

## Minimise to tray

`Hide to tray` puts AudioPush in the notification area and keeps streaming.
Double-click the icon, or right-click → **Show AudioPush**, to bring it back.
**Quit** from the same menu stops the stream and exits.

---

## Troubleshooting

### The sender's receiver list stays empty

1. Is the receiver **started** (not just open)? It only announces while running.
2. Firewall. On **both** PCs, Windows must allow `audiopush.exe` on **Private
   networks**. If you dismissed the prompt, re-add it:
   Settings → Network & Internet → Windows Firewall → *Allow an app through
   firewall* → Change settings → Allow another app → browse to
   `audiopush.exe` → tick **Private**.
3. Both machines on the same subnet? A "guest" WiFi network, or a router with
   AP isolation / client isolation enabled, blocks PC-to-PC traffic entirely.
   Many mesh systems split 2.4 GHz and 5 GHz into separate networks.
4. Press **Scan network**. Some access points drop broadcast packets but pass
   unicast fine, and the scan uses unicast.
5. Still nothing: read the IP off the receiver screen and type it into the
   sender. If that works, it was broadcast filtering.

### "Could not use discovery port 45777"

Another copy of AudioPush is already running on that PC. Only one instance per
machine; check the tray.

### The workstation has no output device at all

WASAPI loopback needs *something* to capture. If Windows shows no playback
device, install [VB-CABLE](https://vb-audio.com/Cable/) (free, tiny), set
**CABLE Input** as the default playback device, and capture from it. Your apps
play into the virtual cable, AudioPush taps it, and the laptop plays it.

### Dropouts, crackling, or stuttering

In this order:

1. Switch the preset to **Balanced**, then **Stable**. The receiver's
   `dropouts` counter should stop rising.
2. Move to 5 GHz WiFi if you are on 2.4 GHz, or plug in Ethernet.
3. Check the counters at the bottom of the receiver screen:
   - **lost** climbing → packets are being dropped by the network. WiFi
     quality, or interference.
   - **dropouts** climbing with no **lost** → the jitter buffer is running
     dry. Use a longer preset.
   - **late** climbing → bursty delivery. Use a longer preset.
4. **Turn off WiFi power saving on the receiver.** This is the single most
   common cause of periodic hiccups on an otherwise healthy 5 GHz link: the
   adapter parks between beacons and delivers a burst of packets late.
   Device Manager → Network adapters → your WiFi adapter → Properties →
   Advanced → set any *Power Saving Mode* / *PnP* / *Selective Suspend* entry
   to maximum performance or Disabled. Then Power Options → your plan →
   Change advanced settings → Wireless Adapter Settings → **Maximum
   Performance**.
5. Close anything saturating the WiFi (large downloads, video calls).
6. Confirm `Pro Audio` appears in the status line. If it does not, Windows
   refused the MMCSS registration — usually some third-party "game booster" or
   audio tweaking tool interfering.

### Audio is delayed but otherwise perfect

That is the preset doing its job. Drop to a shorter one; if dropouts appear,
go back one step. Roughly 25 ms over WiFi and 15 ms over Ethernet is what this
design can do.

### Nothing is captured (sender meter flat)

The chosen endpoint is not the one Windows is playing to. Set **Capture from**
to "Default output (follow Windows)", or pick the exact device shown in the
Windows volume mixer.

### Playback device changed while streaming

Handled: both ends notice within a second and reopen on the new default. You
will hear a brief gap.

---

## How it works

```
workstation                                      laptop
-----------                                      ------
WASAPI loopback   ->  lock-free ring  ->  UDP  ->  jitter buffer  ->  WASAPI render
(Pro Audio thread)    (no allocation)              (1-4 blocks)       (Pro Audio thread)
```

- **Format** 48 kHz, stereo, 16-bit PCM. Raw, uncompressed, ~1.5 Mbit/s.
  No encoder means no encoder delay.
- **Blocks** 240 frames = 5 ms. One block per UDP datagram, 972 bytes
  including a 12-byte header (magic, sequence, timestamp).
- **No acknowledgements.** A lost packet is counted and skipped. Nothing ever
  waits for the network.
- **No allocation in the audio path.** Rings, packet buffers and scratch
  buffers are allocated once at start-up and reused.
- **Thread separation.** Audio threads run at MMCSS "Pro Audio" priority and
  only ever touch a lock-free ring; the UDP threads sit on the other side of
  it. Network trouble cannot stall audio, and audio cannot stall the network.
- **Event-driven WASAPI**, never polling: the thread sleeps on the
  buffer-ready event.
- **Jitter buffer** starts at one device buffer (the quantum WASAPI drains per
  callback) + the preset margin, then **adapts**: it gives back a block after
  12 quiet seconds if the low-water mark says the margin was genuinely spare,
  and takes two blocks back immediately on any dropout. Slow to shrink, fast
  to grow. The receiver shows `buffer <now>/<target> ms` so you can watch it
  settle. If it runs dry it conceals the gap by fading out the last frame
  rather than cutting to zero.
- **Send pacing.** WASAPI hands the capture thread a whole engine period at
  once — two blocks every ~10 ms. Sending both back to back makes arrival
  bursty, and a receiver has to buffer for the gap between bursts, not for the
  average rate. The sender spreads datagrams one block apart instead, which
  costs nothing in latency and lets the buffer run far shallower.
- **Clock drift correction.** The two PCs run off independent crystals, so one
  produces audio slightly faster than the other consumes it — typically
  ~100 ppm, which walks the buffer off target by 20 ms every few minutes and
  would glitch on a perfect network. Once the buffer drifts half a block off
  target the receiver drops or repeats a single 20 µs frame, which is
  inaudible and holds the buffer centred indefinitely.
- **Why not GPU?** The per-packet work is a 960-byte copy. A GPU round trip
  costs more in PCIe latency and launch jitter than the entire audio budget,
  and the CPU cost here is well under 1% of one core. This workload is
  latency-bound, not compute-bound.
- **Silence keep-alive.** When nothing is playing the sender emits silent
  blocks, so the link stays primed and visibly connected.

### Ports

| Port | Protocol | Purpose |
|---|---|---|
| 45777 | UDP | Discovery: receiver announcements, scan probes |
| 45778 | UDP | Audio |

### Project layout

```
src/
  main.rs             entry point, window setup, timer resolution
  app.rs              egui front end: screens, device pickers, meters
  engine.rs           starts/stops the worker threads for one session
  config.rs           constants and the three latency presets
  ring.rs             lock-free SPSC ring of fixed-size audio blocks
  priority.rs         MMCSS "Pro Audio" registration, 1 ms timer resolution
  state.rs            atomics shared between workers and the GUI
  tray.rs             notification-area icon
  audio/
    mod.rs            wire format, shared client setup
    devices.rs        endpoint enumeration for the dropdowns
    capture.rs        WASAPI loopback capture -> ring
    render.rs         ring -> WASAPI render, plus the jitter buffer
  net/
    mod.rs            hostname, local addresses, subnet maths
    protocol.rs       packet encode/decode
    sender.rs         ring -> UDP
    receiver.rs       UDP -> ring, loss detection
    discovery.rs      broadcast announce / listen
    scan.rs           subnet probe fallback
```

Run the tests with `cargo test`.

---

## Using the receiver's microphone

The receiver can lend its microphone back to the sender, so a workstation
with no microphone can use the laptop's or the phone's.

- The switch lives on the **receiver**, because that is the machine whose
  microphone opens. It is **off by default**, and the microphone is opened
  only while streaming with the switch on, and released the moment you stop.
- On the sender, choose where the voice is played under **Use the receiver's
  microphone**.

**Windows cannot expose a virtual microphone from an ordinary application** —
that needs a signed kernel driver. So the voice arrives and is played into an
output device of your choice. To use it as a microphone in other apps:

1. Install [VB-CABLE](https://vb-audio.com/Cable/).
2. In AudioPush, play the microphone into **CABLE Input**.
3. In OBS, Discord or your recorder, select **CABLE Output** as the microphone.

### What protects it

The microphone destination is **latched for the session**: it is set once,
from the first machine that streams audio to the receiver, and never
reassigned. Without that latch, anyone on the network could send a single UDP
packet to the audio port and silently redirect your live microphone to
themselves.

The receiver also **shows where the microphone is going** while it is on, so
a redirection could not be invisible. Stopping and starting the stream is
what re-pairs it.

There is no authentication beyond this. AudioPush assumes a home or office
network you trust: anyone already on it can send audio to your speakers, and
could in principle win the race to be the first peer of a microphone session
if they knew to try at the exact moment you pressed Start. If that matters
for your network, leave the microphone switch off and use a USB microphone.
