//! Wire formats.
//!
//! Audio packets are fixed size, which keeps both ends allocation-free: a
//! datagram is always a 12-byte header followed by exactly one 5 ms block of
//! interleaved int16 little-endian PCM.
//!
//! ```text
//! offset  size  field
//!      0     4  magic  "APU1"
//!      4     4  seq    u32 LE, increments once per block, wraps
//!      8     4  ts     u32 LE, microseconds since the stream started, wraps
//!     12   960  payload, 480 int16 LE samples (240 stereo frames)
//! ```

use crate::config::{BLOCK_BYTES, BLOCK_SAMPLES};

/// Audio packet magic, "APU1".
pub const MAGIC: [u8; 4] = *b"APU1";
pub const HEADER_LEN: usize = 12;
pub const PACKET_LEN: usize = HEADER_LEN + BLOCK_BYTES;

/// Writes one audio packet into a pre-allocated buffer. No allocation.
pub fn encode_audio(out: &mut [u8; PACKET_LEN], seq: u32, ts_us: u32, block: &[i16; BLOCK_SAMPLES]) {
    out[0..4].copy_from_slice(&MAGIC);
    out[4..8].copy_from_slice(&seq.to_le_bytes());
    out[8..12].copy_from_slice(&ts_us.to_le_bytes());
    for (i, sample) in block.iter().enumerate() {
        let offset = HEADER_LEN + i * 2;
        let bytes = sample.to_le_bytes();
        out[offset] = bytes[0];
        out[offset + 1] = bytes[1];
    }
}

/// Parsed audio packet header.
#[derive(Clone, Copy, Debug)]
pub struct AudioHeader {
    pub seq: u32,
    /// Reserved for latency measurement; the jitter buffer works off arrival
    /// order alone, so nothing reads it yet.
    #[allow(dead_code)]
    pub ts_us: u32,
}

/// Validates a received datagram. Returns `None` for anything that is not a
/// well-formed audio packet, so stray traffic on the port is simply ignored.
pub fn decode_audio(data: &[u8]) -> Option<(AudioHeader, &[u8])> {
    if data.len() < PACKET_LEN || data[0..4] != MAGIC {
        return None;
    }
    let seq = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let ts_us = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    Some((AudioHeader { seq, ts_us }, &data[HEADER_LEN..PACKET_LEN]))
}

/// Copies a validated payload into a block of samples. No allocation.
pub fn payload_into_block(payload: &[u8], out: &mut [i16; BLOCK_SAMPLES]) {
    for (i, sample) in out.iter_mut().enumerate() {
        let offset = i * 2;
        *sample = i16::from_le_bytes([payload[offset], payload[offset + 1]]);
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Discovery magic, "APD1".
pub const DISCOVERY_MAGIC: [u8; 4] = *b"APD1";
pub const MSG_ANNOUNCE: u8 = 1;
pub const MSG_PROBE: u8 = 2;
/// Enough for the magic, type, port, name length and a 64-byte hostname.
pub const DISCOVERY_MAX: usize = 4 + 1 + 2 + 1 + 64;

/// Builds a receiver announcement: "I am <hostname>, send audio to <port>".
pub fn encode_announce(hostname: &str, audio_port: u16) -> Vec<u8> {
    let name = hostname.as_bytes();
    let name = &name[..name.len().min(64)];
    let mut out = Vec::with_capacity(8 + name.len());
    out.extend_from_slice(&DISCOVERY_MAGIC);
    out.push(MSG_ANNOUNCE);
    out.extend_from_slice(&audio_port.to_le_bytes());
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out
}

/// Builds a scan probe: "any receivers out there, answer now".
pub fn encode_probe() -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    out.extend_from_slice(&DISCOVERY_MAGIC);
    out.push(MSG_PROBE);
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryMessage {
    Announce { hostname: String, audio_port: u16 },
    Probe,
}

/// Parses a discovery datagram, ignoring anything malformed.
pub fn decode_discovery(data: &[u8]) -> Option<DiscoveryMessage> {
    if data.len() < 5 || data[0..4] != DISCOVERY_MAGIC {
        return None;
    }
    match data[4] {
        MSG_PROBE => Some(DiscoveryMessage::Probe),
        MSG_ANNOUNCE => {
            if data.len() < 8 {
                return None;
            }
            let audio_port = u16::from_le_bytes([data[5], data[6]]);
            let name_len = data[7] as usize;
            if data.len() < 8 + name_len {
                return None;
            }
            let hostname = String::from_utf8_lossy(&data[8..8 + name_len]).into_owned();
            Some(DiscoveryMessage::Announce {
                hostname,
                audio_port,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_roundtrip() {
        let mut block = [0i16; BLOCK_SAMPLES];
        block[0] = -32768;
        block[1] = 32767;
        block[BLOCK_SAMPLES - 1] = 42;
        let mut packet = [0u8; PACKET_LEN];
        encode_audio(&mut packet, 7, 1234, &block);

        let (header, payload) = decode_audio(&packet).expect("valid packet");
        assert_eq!(header.seq, 7);
        assert_eq!(header.ts_us, 1234);
        let mut out = [0i16; BLOCK_SAMPLES];
        payload_into_block(payload, &mut out);
        assert_eq!(out, block);
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(decode_audio(&[]).is_none());
        assert!(decode_audio(&[0u8; PACKET_LEN]).is_none());
        assert!(decode_audio(b"APU1 short").is_none());
        assert!(decode_discovery(b"nope").is_none());
        assert!(decode_discovery(&[b'A', b'P', b'D', b'1', 99]).is_none());
    }

    #[test]
    fn discovery_roundtrip() {
        let bytes = encode_announce("LAPTOP", 45778);
        assert_eq!(
            decode_discovery(&bytes),
            Some(DiscoveryMessage::Announce {
                hostname: "LAPTOP".into(),
                audio_port: 45778
            })
        );
        assert_eq!(
            decode_discovery(&encode_probe()),
            Some(DiscoveryMessage::Probe)
        );
    }
}
