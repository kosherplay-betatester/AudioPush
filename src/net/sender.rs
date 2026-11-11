//! Drains an audio ring into UDP datagrams.
//!
//! Used for both directions: the main stream from the workstation, and the
//! return microphone stream from the receiver. Deliberately dumb: one block
//! per datagram, no acknowledgements, no retransmission and no blocking. If a
//! send fails we count it and carry on, because stalling here would back up
//! into the audio thread.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::{BLOCK_SAMPLES, FRAMES_PER_BLOCK, SAMPLE_RATE};
use crate::net::protocol::{PACKET_LEN, encode_audio};
use crate::pairing::TokenSlot;
use crate::ring::BlockRing;
use crate::state::{Shared, now_ms};

/// How long to nap when the ring is empty. The process raises the system
/// timer resolution to 1 ms at start-up, so this really is a short nap and
/// not a 15 ms stall.
const IDLE_NAP: Duration = Duration::from_micros(250);

/// Wall-clock duration of one block, which is also the pacing interval.
const BLOCK_DURATION: Duration =
    Duration::from_nanos((FRAMES_PER_BLOCK as u64) * 1_000_000_000 / (SAMPLE_RATE as u64));

/// Backlog (in blocks) above which pacing is abandoned so the ring can drain.
/// Pacing is there to smooth delivery, never to hold audio back.
const PACE_BACKLOG: usize = 3;

/// Where to send. A shared slot rather than a fixed address because the
/// microphone stream only learns its destination once the main stream has
/// told the receiver who the sender is.
pub type Target = Arc<Mutex<Option<SocketAddr>>>;

pub fn new_target(addr: Option<SocketAddr>) -> Target {
    Arc::new(Mutex::new(addr))
}

pub fn spawn(
    target: Target,
    token: TokenSlot,
    magic: [u8; 4],
    ring: Arc<BlockRing>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("audiopush-udp-send".into())
        .spawn(move || {
            let socket = match UdpSocket::bind(("0.0.0.0", 0)) {
                Ok(s) => s,
                Err(e) => {
                    shared.set_net_error(Some(format!("could not open a UDP socket: {e}")));
                    return;
                }
            };
            let _ = socket.set_nonblocking(false);

            shared.set_net_error(None);

            let mut packet = [0u8; PACKET_LEN];
            let mut block = [0i16; BLOCK_SAMPLES];
            let mut seq: u32 = 0;
            let started = Instant::now();
            let mut consecutive_errors = 0u32;
            let mut last_peer: Option<SocketAddr> = None;

            // Pacing clock: the time the next datagram is due to leave.
            let mut next_send = Instant::now();

            while !stop.load(Ordering::Relaxed) {
                if !ring.pop(&mut block) {
                    thread::sleep(IDLE_NAP);
                    // Do not let the schedule fall behind while idle.
                    next_send = next_send.max(Instant::now());
                    continue;
                }

                // Both the destination and the key to sign with: the
                // microphone stream learns them together, when the first
                // audio packet tells the receiver who is streaming to it.
                let signing_key = token.lock().ok().and_then(|t| t.clone());
                let (Some(dest), Some(signing_key)) =
                    (target.lock().ok().and_then(|t| *t), signing_key)
                else {
                    // No destination yet. The block has already been taken
                    // from the ring, so it is simply dropped: holding it would
                    // only stale the buffer.
                    continue;
                };
                if last_peer != Some(dest) {
                    shared.set_peer(Some(dest.to_string()));
                    last_peer = Some(dest);
                }

                // --- pacing ------------------------------------------------
                // WASAPI hands the capture thread a whole engine period at
                // once -- two blocks every ~10 ms -- and firing both back to
                // back makes arrival at the far end bursty. Burstiness is
                // what forces a deep jitter buffer: the receiver has to cover
                // the gap between bursts, not the average rate. Spreading the
                // sends one block apart costs nothing in latency (the second
                // block would not have been played any earlier anyway) and
                // lets the receiver run a much shallower buffer.
                if ring.len() <= PACE_BACKLOG {
                    let now = Instant::now();
                    if next_send > now {
                        let wait = next_send - now;
                        if wait <= BLOCK_DURATION * 4 {
                            thread::sleep(wait);
                        } else {
                            // Clock ran away (machine slept, say): resync.
                            next_send = now;
                        }
                    }
                }

                let ts_us = started.elapsed().as_micros() as u32;
                encode_audio(&mut packet, magic, &signing_key, seq, ts_us, &block);
                seq = seq.wrapping_add(1);

                next_send += BLOCK_DURATION;
                // Never let the schedule drift more than a couple of blocks
                // behind real time, or pacing turns into a backlog.
                let now = Instant::now();
                if next_send + BLOCK_DURATION * 2 < now {
                    next_send = now;
                }

                match socket.send_to(&packet, dest) {
                    Ok(_) => {
                        consecutive_errors = 0;
                        shared.blocks.fetch_add(1, Ordering::Relaxed);
                        shared.last_packet_ms.store(now_ms(), Ordering::Relaxed);
                    }
                    Err(e) => {
                        // Common and harmless while the peer is rebooting or
                        // WiFi is renegotiating: report, back off, keep going.
                        consecutive_errors = consecutive_errors.saturating_add(1);
                        if consecutive_errors == 1 || consecutive_errors % 200 == 0 {
                            shared.set_net_error(Some(format!("send failed: {e}")));
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                }
            }
        })
        .expect("failed to spawn udp sender thread")
}
