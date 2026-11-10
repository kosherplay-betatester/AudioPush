//! Receiver side: event-driven WASAPI render plus the jitter buffer.
//!
//! The ring that the UDP receiver fills *is* the jitter buffer. This thread
//! waits until the preset depth has accumulated, then drains it in lockstep
//! with the device. If it ever runs dry it emits silence and re-primes; if it
//! drifts too deep (a WiFi burst delivering a backlog) it drops the oldest
//! blocks so the delay cannot creep upward over a long session.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wasapi::{DeviceEnumerator, Direction};

use crate::audio::{devices, open_client};
use crate::config::{BLOCK_SAMPLES, CHANNELS, FRAMES_PER_BLOCK, Latency};
use crate::priority::ProAudioThread;
use crate::ring::BlockRing;
use crate::state::Shared;

pub fn spawn(
    device_id: Option<String>,
    preset: Latency,
    ring: Arc<BlockRing>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("audiopush-render".into())
        .spawn(move || {
            let _ = wasapi::initialize_mta().ok();
            let pro = ProAudioThread::register();
            shared.pro_audio.store(pro.is_registered(), Ordering::Relaxed);

            while !stop.load(Ordering::Relaxed) {
                match render_once(device_id.as_deref(), preset, &ring, &shared, &stop) {
                    Ok(()) => {}
                    Err(err) => {
                        shared.audio_running.store(false, Ordering::Relaxed);
                        shared.set_audio_error(Some(err));
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
        .expect("failed to spawn render thread")
}

fn render_once(
    device_id: Option<&str>,
    preset: Latency,
    ring: &BlockRing,
    shared: &Shared,
    stop: &AtomicBool,
) -> Result<(), String> {
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    let device = devices::resolve(&enumerator, device_id, Direction::Render)
        .map_err(|e| format!("no playback device: {e}"))?;
    let device_name = device.get_friendlyname().unwrap_or_default();
    let open_id = device.get_id().ok();

    let (client, event, blockalign) = open_client(&device, Direction::Render, preset)
        .map_err(|e| format!("could not open playback: {e}"))?;
    let render_client = client
        .get_audiorenderclient()
        .map_err(|e| format!("could not get render client: {e}"))?;
    let buffer_frames = client.get_buffer_size().unwrap_or(1024) as usize;

    // Pre-allocated once; the loop only ever writes into these.
    let mut scratch = vec![0u8; blockalign * buffer_frames.max(FRAMES_PER_BLOCK) * 4];
    let mut pending = [0i16; BLOCK_SAMPLES];
    // Index into `pending`; == BLOCK_SAMPLES means "no block in hand".
    let mut pending_pos = BLOCK_SAMPLES;

    // The device buffer is the quantum this thread drains per callback, so
    // the jitter target is derived from it rather than being a fixed number
    // of blocks. See `Latency::jitter_target_blocks`.
    //
    // The preset sets the *starting* depth. From there the buffer adapts: it
    // creeps down while the link behaves and jumps back up the moment it does
    // not, so a calm network gets Low-preset latency without a calm network
    // being a requirement. The drift corrector below does the actual moving,
    // one frame at a time, which is why retargeting is inaudible.
    let base_target = preset.jitter_target_blocks(buffer_frames);
    // Never go below one device quantum plus a block of margin.
    let floor_target = (buffer_frames.div_ceil(FRAMES_PER_BLOCK) + 1).max(2);
    let max_target = base_target * 2;
    let mut target = base_target;
    let mut ceiling = target * 2;
    let mut last_trouble = Instant::now();
    let mut last_adapt = Instant::now();
    let mut min_depth_blocks = usize::MAX;
    let mut primed = false;
    let mut peak = 0.0f32;
    // Last sample played on each channel, used to conceal a shortfall.
    let mut last_sample = [0i16; 2];

    // Hand the device a buffer of silence before starting, so playback never
    // begins on whatever was left in the engine buffer.
    if let Ok(frames) = client.get_available_space_in_frames() {
        let bytes = frames as usize * blockalign;
        if bytes > 0 && bytes <= scratch.len() {
            scratch[..bytes].fill(0);
            let _ = render_client.write_to_device(&scratch[..bytes], None);
        }
    }

    client
        .start_stream()
        .map_err(|e| format!("could not start playback: {e}"))?;
    shared.set_audio_error(None);
    shared.set_device_name(Some(device_name));
    shared.audio_running.store(true, Ordering::Relaxed);

    let mut device_check = Instant::now();

    let result: Result<(), String> = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }

        if event.wait_for_event(200).is_err() {
            // Timeout. Nothing to do but re-check the stop flag.
            continue;
        }

        let frames = match client.get_available_space_in_frames() {
            Ok(f) => f as usize,
            Err(e) => break Err(format!("playback failed: {e}")),
        };
        if frames == 0 {
            continue;
        }

        let bytes = frames * blockalign;
        if bytes > scratch.len() {
            scratch.resize(bytes, 0);
        }

        // Keep the buffer from drifting deep: anything beyond the ceiling is
        // stale audio we would otherwise play late forever.
        let depth = ring.len();
        if depth > ceiling {
            let dropped = ring.trim_to(target) as u64;
            shared.dropped_late.fetch_add(dropped, Ordering::Relaxed);
            // Overshooting is trouble too: it means delivery is bursty, and
            // shrinking the buffer now would only make the next burst worse.
            last_trouble = Instant::now();
        }
        shared.buffer_blocks.store(ring.len() as u64, Ordering::Relaxed);
        shared.target_blocks.store(target as u64, Ordering::Relaxed);

        // --- adaptive buffer depth -----------------------------------------
        // Shrink by a block after a quiet spell, but only if the buffer never
        // came close to empty during it: the low-water mark is the honest
        // measure of how much margin was actually spare.
        min_depth_blocks = min_depth_blocks.min(depth);
        if last_adapt.elapsed() > Duration::from_secs(5) {
            last_adapt = Instant::now();
            let quiet = last_trouble.elapsed() > Duration::from_secs(12);
            let spare = min_depth_blocks != usize::MAX && min_depth_blocks >= 2;
            if quiet && spare && target > floor_target {
                target -= 1;
                ceiling = target * 2;
            }
            min_depth_blocks = usize::MAX;
        }

        // Prime: hold silence until the jitter buffer has reached its target.
        if !primed {
            if ring.len() >= target.max(1) {
                primed = true;
            } else {
                scratch[..bytes].fill(0);
                if let Err(e) = render_client.write_to_device(&scratch[..bytes], None) {
                    break Err(format!("playback write failed: {e}"));
                }
                shared.set_level(0.0);
                continue;
            }
        }

        // --- clock drift correction ---------------------------------------
        // The two PCs run off independent crystals, so one produces audio
        // slightly faster than the other consumes it. Left alone the buffer
        // walks to one end and glitches every few minutes, however healthy
        // the network is. Dropping or repeating a single frame (20 us) once
        // the buffer has drifted half a block off target is inaudible, and
        // holds it centred for as long as the stream runs.
        let depth_frames =
            ring.len() * FRAMES_PER_BLOCK + (BLOCK_SAMPLES - pending_pos) / CHANNELS;
        let target_frames = target * FRAMES_PER_BLOCK;
        let tolerance = FRAMES_PER_BLOCK / 2;
        let mut insert_frame = false;
        if depth_frames > target_frames + tolerance {
            // Running deep: skip frames. How many depends on how far off we
            // are -- one frame per callback only corrects ~2000 ppm, and a
            // virtual or software-clocked endpoint can drift further than
            // that. Four frames is still only 83 us, well below audibility.
            let excess = depth_frames - target_frames;
            let drop_frames = (excess / FRAMES_PER_BLOCK).clamp(1, 4);
            for _ in 0..drop_frames {
                if pending_pos + CHANNELS <= BLOCK_SAMPLES {
                    pending_pos += CHANNELS;
                } else if ring.pop(&mut pending) {
                    // The block in hand is spent; take the next one and skip
                    // its first frame. Without this the correction silently
                    // does nothing, because the block in hand is normally
                    // exhausted exactly at the end of a callback.
                    pending_pos = CHANNELS;
                } else {
                    break;
                }
            }
        } else if depth_frames + tolerance < target_frames {
            // Running shallow: repeat one frame.
            insert_frame = true;
        }

        // Fill the device buffer sample by sample from whole ring blocks,
        // carrying any partial block over to the next callback.
        let mut written = 0usize;
        let total_samples = frames * 2; // stereo
        let mut underran = false;

        if insert_frame && total_samples >= CHANNELS {
            for channel in 0..CHANNELS {
                let le = last_sample[channel].to_le_bytes();
                scratch[channel * 2] = le[0];
                scratch[channel * 2 + 1] = le[1];
            }
            written = CHANNELS;
        }
        while written < total_samples {
            if pending_pos >= BLOCK_SAMPLES {
                if !ring.pop(&mut pending) {
                    underran = true;
                    break;
                }
                pending_pos = 0;
            }
            let take = (total_samples - written).min(BLOCK_SAMPLES - pending_pos);
            for i in 0..take {
                let sample = pending[pending_pos + i];
                let magnitude = (sample as f32 / 32768.0).abs();
                if magnitude > peak {
                    peak = magnitude;
                }
                let offset = (written + i) * 2;
                let le = sample.to_le_bytes();
                scratch[offset] = le[0];
                scratch[offset + 1] = le[1];
            }
            if take > 0 {
                // Remember the final frame, in case the next pass has to
                // conceal a gap starting from here.
                let end = pending_pos + take;
                last_sample[1] = pending[end - 1];
                last_sample[0] = if take >= 2 {
                    pending[end - 2]
                } else {
                    pending[end - 1]
                };
            }
            pending_pos += take;
            written += take;
        }

        if underran {
            // Conceal the shortfall instead of cutting straight to zero: a
            // hard edge in the waveform is the click you hear, not the
            // missing milliseconds themselves. Fading the last sample out
            // over ~1 ms turns a click into something inaudible.
            let mut decay = 1.0f32;
            let mut sample_index = written;
            while sample_index < total_samples {
                let channel = sample_index % 2;
                let value = (last_sample[channel] as f32 * decay) as i16;
                let offset = sample_index * 2;
                let le = value.to_le_bytes();
                scratch[offset] = le[0];
                scratch[offset + 1] = le[1];
                if channel == 1 {
                    // ~1 ms to silence at 48 kHz.
                    decay *= 0.98;
                }
                sample_index += 1;
            }
            last_sample = [0, 0];
            shared.underruns.fetch_add(1, Ordering::Relaxed);
            // Running dry means the buffer was too shallow for this link.
            // Give it room back immediately -- adaptation is deliberately
            // slow to shrink and fast to grow.
            target = (target + 2).min(max_target);
            ceiling = target * 2;
            last_trouble = Instant::now();
            min_depth_blocks = usize::MAX;
            // Re-prime, but keep the partially played block: throwing it away
            // would discard audio we already have in hand.
            primed = false;
        }

        if let Err(e) = render_client.write_to_device(&scratch[..bytes], None) {
            break Err(format!("playback write failed: {e}"));
        }
        shared.set_level(peak);
        peak *= 0.6; // gentle decay so the meter does not stick at a transient

        if device_id.is_none() && device_check.elapsed() > Duration::from_secs(1) {
            device_check = Instant::now();
            if let Ok(current) = enumerator.get_default_device(Direction::Render) {
                if let Ok(current_id) = current.get_id() {
                    if open_id.as_deref() != Some(current_id.as_str()) {
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
