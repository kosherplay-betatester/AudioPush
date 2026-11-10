//! State shared between the worker threads and the GUI.
//!
//! The GUI only ever reads atomics (plus a couple of short-lived mutexes for
//! strings), so drawing a frame can never stall an audio or network thread.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch. Used for "last seen" bookkeeping.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
pub struct Shared {
    /// Peak sample level of the most recent block, as f32 bits (0.0 - 1.0).
    level_bits: AtomicU32,
    /// Audio blocks sent or received since the stream started.
    pub blocks: AtomicU64,
    /// Packets detected as missing from the sequence numbers.
    pub lost: AtomicU64,
    /// Times the renderer had to fill with silence because the buffer ran dry.
    pub underruns: AtomicU64,
    /// Blocks dropped because the jitter buffer had drifted too deep.
    pub dropped_late: AtomicU64,
    /// `now_ms()` of the last audio packet seen on the wire.
    pub last_packet_ms: AtomicU64,
    /// Current jitter-buffer occupancy in blocks (receiver only).
    pub buffer_blocks: AtomicU64,
    /// Depth the jitter buffer is currently aiming for, in blocks. Moves on
    /// its own as the receiver adapts to the link.
    pub target_blocks: AtomicU64,
    /// True once the audio device is open and running.
    pub audio_running: AtomicBool,
    /// True when MMCSS accepted the Pro Audio registration.
    pub pro_audio: AtomicBool,
    /// Last error from the audio thread, if any.
    pub audio_error: Mutex<Option<String>>,
    /// Last error from the network thread, if any.
    pub net_error: Mutex<Option<String>>,
    /// Human-readable peer description, e.g. "LAPTOP (192.168.1.20:45778)".
    pub peer: Mutex<Option<String>>,
    /// Name of the audio device actually in use.
    pub device_name: Mutex<Option<String>>,
}

impl Shared {
    pub fn new() -> Shared {
        Shared::default()
    }

    pub fn set_level(&self, peak: f32) {
        self.level_bits.store(peak.to_bits(), Ordering::Relaxed);
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed))
    }

    pub fn set_audio_error(&self, msg: Option<String>) {
        if let Ok(mut guard) = self.audio_error.lock() {
            *guard = msg;
        }
    }

    pub fn set_net_error(&self, msg: Option<String>) {
        if let Ok(mut guard) = self.net_error.lock() {
            *guard = msg;
        }
    }

    pub fn set_peer(&self, msg: Option<String>) {
        if let Ok(mut guard) = self.peer.lock() {
            *guard = msg;
        }
    }

    pub fn set_device_name(&self, msg: Option<String>) {
        if let Ok(mut guard) = self.device_name.lock() {
            *guard = msg;
        }
    }

    pub fn get_string(lock: &Mutex<Option<String>>) -> Option<String> {
        lock.lock().ok().and_then(|g| g.clone())
    }

    /// True when an audio packet arrived recently enough to call the link live.
    pub fn stream_live(&self, timeout_ms: u64) -> bool {
        let last = self.last_packet_ms.load(Ordering::Relaxed);
        last != 0 && now_ms().saturating_sub(last) < timeout_ms
    }

    /// Resets the per-session counters without disturbing error strings.
    pub fn reset_counters(&self) {
        self.blocks.store(0, Ordering::Relaxed);
        self.lost.store(0, Ordering::Relaxed);
        self.underruns.store(0, Ordering::Relaxed);
        self.dropped_late.store(0, Ordering::Relaxed);
        self.last_packet_ms.store(0, Ordering::Relaxed);
        self.buffer_blocks.store(0, Ordering::Relaxed);
        self.target_blocks.store(0, Ordering::Relaxed);
        self.set_level(0.0);
    }
}

/// A receiver the sender has heard from, either via broadcast or a scan reply.
#[derive(Clone, Debug)]
pub struct Peer {
    pub hostname: String,
    pub addr: SocketAddr,
    pub last_seen_ms: u64,
}
