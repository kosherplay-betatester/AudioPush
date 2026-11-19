//! Sender side: event-driven WASAPI loopback capture.
//!
//! The thread registers itself as Pro Audio, opens the chosen *output*
//! endpoint in loopback mode and copies whatever Windows is playing into the
//! lock-free ring, one 5 ms block at a time. The only thing downstream of it
//! is the UDP sender thread, so nothing here can ever be blocked by the
//! network.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wasapi::{DeviceEnumerator, Direction};

use crate::audio::{devices, open_client};
use crate::config::{BLOCK_SAMPLES, FRAMES_PER_BLOCK, Latency, SAMPLE_RATE};
use crate::priority::ProAudioThread;
use crate::ring::BlockRing;
use crate::state::Shared;

/// Wall-clock duration of one block.
const BLOCK_DURATION: Duration =
    Duration::from_nanos((FRAMES_PER_BLOCK as u64) * 1_000_000_000 / (SAMPLE_RATE as u64));

/// After this long without captured audio we assume the system has gone quiet
/// and start emitting silence, so the link stays primed and visibly alive.
/// It has to be comfortably longer than the WASAPI engine period (~10 ms).
const SILENCE_AFTER: Duration = Duration::from_millis(30);

pub fn spawn(
    device_id: Option<String>,
    preset: Latency,
    ring: Arc<BlockRing>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("audiopush-capture".into())
        .spawn(move || {
            // COM for this thread; MTA because we never pump a message loop here.
            let _ = wasapi::initialize_mta().ok();
            let pro = ProAudioThread::register();
            shared.pro_audio.store(pro.is_registered(), Ordering::Relaxed);

            // Outer loop: reopen the device after any failure, which is also
            // how a default-device change or an unplugged endpoint recovers.
            while !stop.load(Ordering::Relaxed) {
                match capture_once(device_id.as_deref(), preset, &ring, &shared, &stop) {
                    Ok(()) => {}
                    Err(err) => {
                        shared.audio_running.store(false, Ordering::Relaxed);
                        shared.set_audio_error(Some(err));
                        // Give Windows a moment before trying again.
                        for _ in 0..10 {
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            }
            shared.audio_running.store(false, Ordering::Relaxed);
        })
        .expect("failed to spawn capture thread")
}

fn capture_once(
    device_id: Option<&str>,
    preset: Latency,
    ring: &BlockRing,
    shared: &Shared,
    stop: &AtomicBool,
) -> Result<(), String> {
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    // Loopback reads from a *render* endpoint; a microphone is an ordinary
    // capture endpoint. The wasapi crate turns the first combination into
    // AUDCLNT_STREAMFLAGS_LOOPBACK on its own.
    let device = devices::resolve(&enumerator, device_id, Direction::Render)
        .map_err(|e| format!("no output device to capture: {e}"))?;
    let device_name = device.get_friendlyname().unwrap_or_default();
    let open_id = device.get_id().ok();

    let (client, event, blockalign) = open_client(&device, Direction::Capture, preset)
        .map_err(|e| format!("could not open output device to capture: {e}"))?;
    let capture_client = client
        .get_audiocaptureclient()
        .map_err(|e| format!("could not get capture client: {e}"))?;
    let buffer_frames = client.get_buffer_size().unwrap_or(1024) as usize;

    // Everything the hot loop touches is allocated here, once.
    let mut scratch = vec![0u8; blockalign * buffer_frames.max(FRAMES_PER_BLOCK) * 4];
    let mut block = [0i16; BLOCK_SAMPLES];
    let silence = [0i16; BLOCK_SAMPLES];
    let mut fill = 0usize;
    let mut peak = 0.0f32;

    client
        .start_stream()
        .map_err(|e| format!("could not start capture: {e}"))?;
    shared.set_audio_error(None);
    shared.set_device_name(Some(device_name));
    shared.audio_running.store(true, Ordering::Relaxed);

    let mut last_emit = Instant::now();
    let mut device_check = Instant::now();

    let result: Result<(), String> = 'session: loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }

        // Wait for the buffer-ready event. A timeout is normal: loopback
        // endpoints go quiet when nothing is playing.
        let woke = event.wait_for_event(100).is_ok();

        if woke {
            loop {
                let frames = match capture_client.get_next_packet_size() {
                    Ok(Some(f)) => f as usize,
                    Ok(None) => 0,
                    Err(e) => break 'session Err(format!("capture failed: {e}")),
                };
                if frames == 0 {
                    break;
                }
                let bytes = frames * blockalign;
                if bytes > scratch.len() {
                    // Only if the engine hands us an unusually large packet;
                    // never in steady state.
                    scratch.resize(bytes, 0);
                }
                let info = match capture_client.read_from_device(&mut scratch[..bytes]) {
                    Ok((_frames_read, info)) => info,
                    Err(e) => break 'session Err(format!("capture read failed: {e}")),
                };
                let silent = info.flags.silent;

                for sample_bytes in scratch[..bytes].chunks_exact(2) {
                    let sample = if silent {
                        0
                    } else {
                        i16::from_le_bytes([sample_bytes[0], sample_bytes[1]])
                    };
                    let magnitude = (sample as f32 / 32768.0).abs();
                    if magnitude > peak {
                        peak = magnitude;
                    }
                    block[fill] = sample;
                    fill += 1;
                    if fill == BLOCK_SAMPLES {
                        // A full ring means the network thread has stalled;
                        // drop the block rather than stall the audio thread.
                        ring.push(&block);
                        shared.set_level(peak);
                        peak = 0.0;
                        fill = 0;
                        last_emit = Instant::now();
                    }
                }
            }
        }

        // Nothing is playing: keep the stream flowing with silence so the
        // receiver stays primed and the status stays "streaming".
        if last_emit.elapsed() > SILENCE_AFTER {
            // After a long stall (the machine slept, say) do not try to make
            // up the whole gap in silence; just restart the clock.
            if last_emit.elapsed() > Duration::from_secs(1) {
                last_emit = Instant::now() - BLOCK_DURATION;
            }
            while last_emit.elapsed() >= BLOCK_DURATION {
                ring.push(&silence);
                last_emit += BLOCK_DURATION;
            }
            shared.set_level(0.0);
        }

        // Cheap periodic check: did Windows switch the default output on us?
        if device_id.is_none() && device_check.elapsed() > Duration::from_secs(1) {
            device_check = Instant::now();
            if let Ok(current) = enumerator.get_default_device(Direction::Render) {
                if let Ok(current_id) = current.get_id() {
                    if open_id.as_deref() != Some(current_id.as_str()) {
                        // Reopen against the new default.
                        break Ok(());
                    }
                }
            }
        }
    };

    let _ = client.stop_stream();
    shared.audio_running.store(false, Ordering::Relaxed);
    result
}
