//! WASAPI capture and render.

pub mod capture;
pub mod devices;
pub mod render;

use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

use crate::config::{BITS, CHANNELS, Latency, SAMPLE_RATE};

/// The format we always ask WASAPI for. Shared-mode autoconversion means the
/// endpoint can be running at any rate or depth and we still get 48 kHz
/// stereo int16, which is exactly what goes on the wire.
pub fn wire_format() -> WaveFormat {
    WaveFormat::new(BITS, BITS, SampleType::Int, SAMPLE_RATE, CHANNELS, None)
}

/// Opens an audio client on `device` for the given stream direction.
///
/// For loopback capture, pass a *render* endpoint with `stream_direction`
/// `Direction::Capture`; the wasapi crate turns that combination into
/// `AUDCLNT_STREAMFLAGS_LOOPBACK` for us.
///
/// Returns the initialised client, its buffer-ready event and the block
/// alignment (bytes per frame).
pub fn open_client(
    device: &wasapi::Device,
    stream_direction: Direction,
    preset: Latency,
) -> Result<(wasapi::AudioClient, wasapi::Handle, usize), wasapi::WasapiError> {
    let format = wire_format();
    let blockalign = format.get_blockalign() as usize;

    let mut client = device.get_iaudioclient()?;
    let (default_hns, min_hns) = client.get_device_period()?;

    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: preset.buffer_hns(default_hns, min_hns),
    };
    client.initialize_client(&format, stream_direction, mode, None)?;

    let event = client.set_get_eventhandle()?;
    Ok((client, event, blockalign))
}
