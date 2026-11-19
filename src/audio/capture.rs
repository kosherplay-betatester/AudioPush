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

/// What the capture thread is listening to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// WASAPI loopback on a render endpoint: whatever Windows is playing.
    SystemAudio,
    /// A real capture endpoint: a microphone or line input.
    Microphone,
}

impl Source {
    /// Which kind of endpoint the device id refers to.
    fn device_direction(&self) -> Direction {
        match self {
            Source::SystemAudio => Direction::Render,
            Source::Microphone => Direction::Capture,
        }
    }

    fn describe(&self) -> &'static str {
        match self {
            Source::SystemAudio => "output device to capture",
            Source::Microphone => "microphone",
        }
    }
}

pub fn spawn(
    device_id: Option<String>,
    source: Source,
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
                match capture_once(device_id.as_deref(), source, preset, &ring, &shared, &stop) {
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
    source: Source,
    preset: Latency,
    ring: &BlockRing,
    shared: &Shared,
    stop: &AtomicBool,
) -> Result<(), String> {
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    // Loopback reads from a *render* endpoint; a microphone is an ordinary
    // capture endpoint. The wasapi crate turns the first combination into
    // AUDCLNT_STREAMFLAGS_LOOPBACK on its own.
    let device = devices::resolve(&enumerator, device_id, source.device_direction())
        .map_err(|e| format!("no {}: {e}", source.describe()))?;
    let device_name = device.get_friendlyname().unwrap_or_default();
    let open_id = device.get_id().ok();

    let (client, event, blockalign) = open_client(&device, Direction::Capture, preset)
        .map_err(|e| format!("could not open {}: {e}", source.describe()))?;
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
            if let Ok(current) = enumerator.get_default_device(source.device_direction()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasapi::DeviceEnumerator;

    /// Measures whether a virtual audio cable changes the pitch of what
    /// passes through it.
    ///
    /// Ignored by default: it needs a cable whose playback side and recording
    /// side are two halves of one device, and it makes a noise. Run with
    /// `cargo test --release -- --ignored --nocapture`.
    ///
    /// A tone goes in at a known frequency and is measured coming out. A
    /// cable whose two halves run at different sample rates and does not
    /// resample between them returns the tone at the wrong pitch, and
    /// everything through it plays at the wrong speed.
    #[test]
    #[ignore]
    fn a_virtual_cable_returns_the_pitch_it_was_given() {
        const TONE_HZ: f64 = 1000.0;
        wasapi::initialize_mta().ok();
        let enumerator = DeviceEnumerator::new().unwrap();

        let speaker = enumerator
            .get_device_collection(Direction::Render)
            .unwrap();
        let mut sink = None;
        for i in 0..speaker.get_nbr_devices().unwrap() {
            let d = speaker.get_device_at_index(i).unwrap();
            let name = d.get_friendlyname().unwrap_or_default();
            if name.contains("Steam Streaming Microphone") || name.contains("CABLE Input") {
                println!("playing into: {name}");
                sink = Some(d);
                break;
            }
        }
        let Some(sink) = sink else {
            println!("no virtual cable found; nothing to measure");
            return;
        };

        let source = enumerator.get_default_device(Direction::Capture).unwrap();
        println!("recording from: {}", source.get_friendlyname().unwrap_or_default());

        let (out_client, out_event, _) =
            crate::audio::open_client(&sink, Direction::Render, Latency::Stable).unwrap();
        let render = out_client.get_audiorenderclient().unwrap();
        let (in_client, in_event, in_align) =
            crate::audio::open_client(&source, Direction::Capture, Latency::Stable).unwrap();
        let capture = in_client.get_audiocaptureclient().unwrap();

        // Both clients live on this thread: a WASAPI client is not Send, and
        // the point is the measurement, not the structure.
        out_client.start_stream().unwrap();
        in_client.start_stream().unwrap();

        let mut phase = 0.0f64;
        let step = std::f64::consts::TAU * TONE_HZ / SAMPLE_RATE as f64;
        let mut tone = vec![0u8; 4 * 4096];

        let started = Instant::now();
        let mut scratch = vec![0u8; 1 << 16];
        let mut crossings = 0usize;
        let mut samples = 0usize;
        let mut previous = 0i16;

        while started.elapsed() < Duration::from_secs(4) {
            // Keep the tone flowing.
            if let Ok(space) = out_client.get_available_space_in_frames() {
                let frames = (space as usize).min(2048);
                for chunk in tone.chunks_exact_mut(4).take(frames) {
                    let v = ((phase.sin() * 12000.0) as i16).to_le_bytes();
                    chunk[..2].copy_from_slice(&v);
                    chunk[2..].copy_from_slice(&v);
                    phase += step;
                }
                if frames > 0 {
                    let _ = render.write_to_device(&tone[..frames * 4], None);
                }
            }

            if in_event.wait_for_event(20).is_err() {
                continue;
            }
            while let Ok(Some(frames)) = capture.get_next_packet_size() {
                if frames == 0 {
                    break;
                }
                let bytes = frames as usize * in_align;
                if bytes > scratch.len() {
                    scratch.resize(bytes, 0);
                }
                if capture.read_from_device(&mut scratch[..bytes]).is_err() {
                    break;
                }
                // Let the tone reach the far side before counting.
                if started.elapsed() < Duration::from_secs(1) {
                    continue;
                }
                for frame in scratch[..bytes].chunks_exact(in_align) {
                    let left = i16::from_le_bytes([frame[0], frame[1]]);
                    if previous < 0 && left >= 0 {
                        crossings += 1;
                    }
                    previous = left;
                    samples += 1;
                }
            }
        }
        let _ = out_client.stop_stream();
        let _ = in_client.stop_stream();

        let seconds = samples as f64 / SAMPLE_RATE as f64;
        let measured = crossings as f64 / seconds;
        println!("sent {TONE_HZ} Hz, measured {measured:.0} Hz over {seconds:.1}s");
        println!("speed through the cable: {:.3}x", measured / TONE_HZ);
    }

    /// Reports what the microphone path really delivers.
    ///
    /// Ignored by default because it needs a real microphone. Run it with
    /// `cargo test --release -- --ignored --nocapture` on a machine that has
    /// one. The number that matters is frames per second: the capture loop
    /// assumes every stream is 48 kHz stereo, and anything else is played
    /// back at the wrong speed.
    #[test]
    #[ignore]
    fn the_microphone_really_arrives_as_48k_stereo() {
        wasapi::initialize_mta().ok().unwrap();
        let enumerator = DeviceEnumerator::new().unwrap();
        let device = enumerator.get_default_device(Direction::Capture).unwrap();
        println!("device: {}", device.get_friendlyname().unwrap_or_default());

        let probe = device.get_iaudioclient().unwrap();
        let mix = probe.get_mixformat().unwrap();
        println!(
            "endpoint mix format: {} Hz, {} ch, {} bits",
            mix.get_samplespersec(),
            mix.get_nchannels(),
            mix.get_bitspersample()
        );

        let (client, event, blockalign) =
            crate::audio::open_client(&device, Direction::Capture, Latency::Balanced).unwrap();
        println!("we asked for blockalign {blockalign} bytes/frame");

        let capture_client = client.get_audiocaptureclient().unwrap();
        client.start_stream().unwrap();

        let started = Instant::now();
        let mut frames_seen = 0usize;
        let mut scratch = vec![0u8; 1 << 16];
        while started.elapsed() < Duration::from_secs(3) {
            if event.wait_for_event(200).is_err() {
                continue;
            }
            while let Ok(Some(frames)) = capture_client.get_next_packet_size() {
                if frames == 0 {
                    break;
                }
                let bytes = frames as usize * blockalign;
                if bytes > scratch.len() {
                    scratch.resize(bytes, 0);
                }
                if capture_client.read_from_device(&mut scratch[..bytes]).is_err() {
                    break;
                }
                frames_seen += frames as usize;
            }
        }
        let _ = client.stop_stream();

        let per_second = frames_seen as f64 / started.elapsed().as_secs_f64();
        println!("frames per second: {per_second:.0}  (expected {SAMPLE_RATE})");
        let ratio = per_second / SAMPLE_RATE as f64;
        println!("playback speed would be {ratio:.3}x");
        assert!(
            (0.95..1.05).contains(&ratio),
            "the capture stream is not 48 kHz stereo: audio would play at {ratio:.3}x speed"
        );
    }
}
