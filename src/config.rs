//! Fixed audio/network constants and the three latency presets.
//!
//! Everything here is a compile-time constant so that buffers can be plain
//! arrays: the audio hot path never allocates.

/// Wire sample rate. Both ends always run at this rate; WASAPI shared-mode
/// autoconversion adapts the actual device rate for us.
pub const SAMPLE_RATE: usize = 48_000;
/// Wire channel count (stereo).
pub const CHANNELS: usize = 2;
/// Bits per sample on the wire (int16 PCM).
pub const BITS: usize = 16;

/// Frames per network block. 240 frames @ 48 kHz = 5.0 ms of audio.
/// One block = one UDP datagram, so this is the transport granularity.
pub const FRAMES_PER_BLOCK: usize = 240;
/// Interleaved i16 samples in one block.
pub const BLOCK_SAMPLES: usize = FRAMES_PER_BLOCK * CHANNELS; // 480
/// Payload bytes in one block.
pub const BLOCK_BYTES: usize = BLOCK_SAMPLES * 2; // 960
/// Milliseconds of audio in one block (for UI maths).
pub const BLOCK_MS: f32 = (FRAMES_PER_BLOCK as f32) * 1000.0 / (SAMPLE_RATE as f32);

/// UDP port carrying audio datagrams.
pub const AUDIO_PORT: u16 = 45778;
/// UDP port used for broadcast announcements and scan probes.
pub const DISCOVERY_PORT: u16 = 45777;

/// Capacity (in blocks) of each lock-free ring. Must be a power of two.
/// 512 blocks = 2.5 s; it is only a safety margin, the pipeline actively keeps
/// occupancy down to the jitter-buffer target.
pub const RING_BLOCKS: usize = 512;

/// How often the receiver announces itself.
pub const ANNOUNCE_INTERVAL_MS: u64 = 1000;
/// A peer leaves the sender list after this long without an announce.
pub const PEER_TIMEOUT_MS: u64 = 4000;
/// The receiver reports "disconnected" after this long with no audio packet.
pub const STREAM_TIMEOUT_MS: u64 = 1000;

/// Latency preset chosen in the GUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Latency {
    /// Smallest WASAPI buffer, jitter buffer of a single block.
    Low,
    /// Default device period, two blocks of jitter buffer.
    Balanced,
    /// Roomier device buffer and four blocks of jitter buffer.
    Stable,
}

impl Latency {
    pub const ALL: [Latency; 3] = [Latency::Low, Latency::Balanced, Latency::Stable];

    pub fn label(&self) -> &'static str {
        match self {
            Latency::Low => "Low",
            Latency::Balanced => "Balanced",
            Latency::Stable => "Stable",
        }
    }

    pub fn hint(&self) -> &'static str {
        match self {
            Latency::Low => "Low = least delay, but a brief WiFi hiccup is audible as a dropout.",
            Latency::Balanced => {
                "Balanced = starts safe, then tightens itself up while the link behaves.                  Recommended."
            }
            Latency::Stable => "Stable = most delay, rides out flaky WiFi without dropouts.",
        }
    }

    /// Buffering held *beyond* one device period, in milliseconds. This is
    /// the part that actually absorbs network jitter.
    pub fn jitter_margin_ms(&self) -> f32 {
        match self {
            Latency::Low => 8.0,
            Latency::Balanced => 20.0,
            Latency::Stable => 50.0,
        }
    }

    /// *Starting* jitter-buffer depth in blocks, for a device whose buffer
    /// holds `device_buffer_frames`. The receiver adapts from here: it
    /// shrinks the buffer while the link behaves and grows it on any dropout,
    /// so this is a safe opening bid rather than a fixed setting.
    ///
    /// This has to account for the device period, not just for the network.
    /// WASAPI hands the render thread a whole device buffer to fill at once,
    /// so the jitter buffer is drained in that quantum: a target smaller than
    /// the quantum underruns on *every* callback, however good the network
    /// is. Target = one quantum (so a callback can always be served) + the
    /// preset margin (so late packets have somewhere to be late).
    pub fn jitter_target_blocks(&self, device_buffer_frames: usize) -> usize {
        let margin_frames = (self.jitter_margin_ms() * SAMPLE_RATE as f32 / 1000.0) as usize;
        (device_buffer_frames + margin_frames).div_ceil(FRAMES_PER_BLOCK).max(2)
    }

    /// WASAPI buffer duration in 100 ns units, derived from the device report.
    pub fn buffer_hns(&self, default_hns: i64, min_hns: i64) -> i64 {
        match self {
            Latency::Low => min_hns,
            Latency::Balanced => default_hns,
            Latency::Stable => default_hns * 3,
        }
    }

    /// Rough end-to-end latency estimate for the UI, in milliseconds:
    /// capture buffer + one block on the wire + the jitter margin + the
    /// render quantum. The receiver screen shows the real measured depth.
    pub fn estimate_ms(&self) -> f32 {
        let device_ms = match self {
            Latency::Low => 3.0,
            Latency::Balanced => 10.0,
            Latency::Stable => 30.0,
        };
        device_ms + BLOCK_MS + self.jitter_margin_ms() + device_ms * 2.0
    }
}
