//! Wire formats.
//!
//! Audio packets are fixed size, which keeps both ends allocation-free: a
//! datagram is always a 16-byte header, exactly one 5 ms block of interleaved
//! int16 little-endian PCM, and an 8-byte authentication tag.
//!
//! ```text
//! offset  size  field
//!      0     4  magic   "APU1" main stream, "APM1" return microphone
//!      4     4  seq     u32 LE, increments once per block, wraps
//!      8     4  ts      u32 LE, microseconds since the stream started
//!     12     4  key_id  u32 LE, which pairing token signed this
//!     16   960  payload, 480 int16 LE samples (240 stereo frames)
//!    976     8  tag     HMAC-SHA256(token, bytes 0..976), truncated
//! ```
//!
//! The tag is what stops an unpaired machine on the network playing audio
//! through your speakers or redirecting your microphone to itself. `key_id`
//! is a non-secret fingerprint of the token, so a receiver paired with
//! several senders picks the right key in one step rather than trying each.

use crate::config::{BLOCK_BYTES, BLOCK_SAMPLES};
use crate::pairing::{TAG_LEN, Token};

/// Magic for the main stream, sender to receiver.
pub const MAGIC: [u8; 4] = *b"APU1";

pub const HEADER_LEN: usize = 16;
/// Everything the tag covers: header plus payload.
pub const SIGNED_LEN: usize = HEADER_LEN + BLOCK_BYTES;
pub const PACKET_LEN: usize = SIGNED_LEN + TAG_LEN;

/// Writes and signs one audio packet in a pre-allocated buffer. No allocation.
pub fn encode_audio(
    out: &mut [u8; PACKET_LEN],
    magic: [u8; 4],
    token: &Token,
    seq: u32,
    ts_us: u32,
    block: &[i16; BLOCK_SAMPLES],
) {
    out[0..4].copy_from_slice(&magic);
    out[4..8].copy_from_slice(&seq.to_le_bytes());
    out[8..12].copy_from_slice(&ts_us.to_le_bytes());
    out[12..16].copy_from_slice(&token.key_id().to_le_bytes());
    for (i, sample) in block.iter().enumerate() {
        let offset = HEADER_LEN + i * 2;
        let bytes = sample.to_le_bytes();
        out[offset] = bytes[0];
        out[offset + 1] = bytes[1];
    }
    let tag = token.tag(&out[..SIGNED_LEN]);
    out[SIGNED_LEN..PACKET_LEN].copy_from_slice(&tag);
}

/// Parsed audio packet header.
#[derive(Clone, Copy, Debug)]
pub struct AudioHeader {
    pub seq: u32,
    /// Reserved for latency measurement; the jitter buffer works off arrival
    /// order alone, so nothing reads it yet.
    #[allow(dead_code)]
    pub ts_us: u32,
    /// Which pairing token the sender claims to have signed with.
    pub key_id: u32,
}

/// Checks shape and magic only. The caller then looks up the token named by
/// `key_id` and calls [`verify_audio`]; nothing is trusted until it does.
pub fn decode_audio(data: &[u8], magic: [u8; 4]) -> Option<(AudioHeader, &[u8])> {
    if data.len() < PACKET_LEN || data[0..4] != magic {
        return None;
    }
    let seq = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let ts_us = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let key_id = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    Some((
        AudioHeader { seq, ts_us, key_id },
        &data[HEADER_LEN..SIGNED_LEN],
    ))
}

/// True when this packet really was signed by `token`.
pub fn verify_audio(data: &[u8], token: &Token) -> bool {
    data.len() >= PACKET_LEN && token.verify(&data[..SIGNED_LEN], &data[SIGNED_LEN..PACKET_LEN])
}

/// Copies a verified payload into a block of samples. No allocation.
pub fn payload_into_block(payload: &[u8], out: &mut [i16; BLOCK_SAMPLES]) {
    for (i, sample) in out.iter_mut().enumerate() {
        let offset = i * 2;
        *sample = i16::from_le_bytes([payload[offset], payload[offset + 1]]);
    }
}

// ---------------------------------------------------------------------------
// Discovery and pairing
// ---------------------------------------------------------------------------

/// Discovery magic, "APD1".
pub const DISCOVERY_MAGIC: [u8; 4] = *b"APD1";
pub const MSG_ANNOUNCE: u8 = 1;
pub const MSG_PROBE: u8 = 2;
pub const MSG_PAIR_REQUEST: u8 = 3;
pub const MSG_PAIR_ACCEPT: u8 = 4;
pub const MSG_PAIR_REJECT: u8 = 5;
/// Enough for the longest message, a pair accept.
pub const DISCOVERY_MAX: usize = 160;

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

/// Sender to receiver: "here is my nonce and proof that I know your PIN".
///
/// The PIN itself never crosses the wire; only a MAC over the nonce does, so
/// a wrong guess tells the attacker nothing beyond "wrong".
pub fn encode_pair_request(nonce: &[u8; 16], proof: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + 16 + 32);
    out.extend_from_slice(&DISCOVERY_MAGIC);
    out.push(MSG_PAIR_REQUEST);
    out.extend_from_slice(nonce);
    out.extend_from_slice(proof);
    out
}

/// Receiver to sender: "correct, here is the token to sign packets with".
pub fn encode_pair_accept(token: &Token, hostname: &str, proof: &[u8; 32]) -> Vec<u8> {
    let name = hostname.as_bytes();
    let name = &name[..name.len().min(64)];
    let mut out = Vec::with_capacity(6 + 32 + 32 + name.len());
    out.extend_from_slice(&DISCOVERY_MAGIC);
    out.push(MSG_PAIR_ACCEPT);
    out.extend_from_slice(token.as_bytes());
    out.extend_from_slice(proof);
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out
}

/// Receiver to sender: "no", with a reason the UI can explain.
pub fn encode_pair_reject(reason: PairReject) -> Vec<u8> {
    let mut out = Vec::with_capacity(6);
    out.extend_from_slice(&DISCOVERY_MAGIC);
    out.push(MSG_PAIR_REJECT);
    out.push(reason as u8);
    out
}

/// Why a pairing attempt was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairReject {
    WrongPin = 1,
    TooManyAttempts = 2,
    /// The receiver is not in pairing mode, so nothing is being answered.
    NotOpen = 3,
}

impl PairReject {
    pub fn from_code(code: u8) -> PairReject {
        match code {
            2 => PairReject::TooManyAttempts,
            3 => PairReject::NotOpen,
            _ => PairReject::WrongPin,
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            PairReject::WrongPin => "Wrong PIN. Check the digits shown on the receiver.",
            PairReject::TooManyAttempts => {
                "Too many wrong attempts. The receiver has stopped answering for a while;                  open pairing on it again."
            }
            PairReject::NotOpen => {
                "The receiver is not in pairing mode. Press 'Pair a new device' on it first."
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryMessage {
    Announce {
        hostname: String,
        audio_port: u16,
    },
    Probe,
    PairRequest {
        nonce: [u8; 16],
        proof: [u8; 32],
    },
    PairAccept {
        token: Token,
        proof: [u8; 32],
        hostname: String,
    },
    PairReject(PairReject),
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
        MSG_PAIR_REQUEST => {
            if data.len() < 5 + 16 + 32 {
                return None;
            }
            let mut nonce = [0u8; 16];
            nonce.copy_from_slice(&data[5..21]);
            let mut proof = [0u8; 32];
            proof.copy_from_slice(&data[21..53]);
            Some(DiscoveryMessage::PairRequest { nonce, proof })
        }
        MSG_PAIR_ACCEPT => {
            if data.len() < 5 + 32 + 32 + 1 {
                return None;
            }
            let token = Token::from_bytes(&data[5..37])?;
            let mut proof = [0u8; 32];
            proof.copy_from_slice(&data[37..69]);
            let name_len = data[69] as usize;
            if data.len() < 70 + name_len {
                return None;
            }
            let hostname = String::from_utf8_lossy(&data[70..70 + name_len]).into_owned();
            Some(DiscoveryMessage::PairAccept {
                token,
                proof,
                hostname,
            })
        }
        MSG_PAIR_REJECT => {
            if data.len() < 6 {
                return None;
            }
            Some(DiscoveryMessage::PairReject(PairReject::from_code(data[5])))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_round_trips_and_is_authenticated() {
        let token = Token::random();
        let mut block = [0i16; BLOCK_SAMPLES];
        block[0] = -32768;
        block[1] = 32767;
        block[BLOCK_SAMPLES - 1] = 42;
        let mut packet = [0u8; PACKET_LEN];
        encode_audio(&mut packet, MAGIC, &token, 7, 1234, &block);

        let (header, payload) = decode_audio(&packet, MAGIC).expect("valid packet");
        assert_eq!(header.seq, 7);
        assert_eq!(header.ts_us, 1234);
        assert_eq!(header.key_id, token.key_id());
        assert!(verify_audio(&packet, &token));

        let mut out = [0i16; BLOCK_SAMPLES];
        payload_into_block(payload, &mut out);
        assert_eq!(out, block);
    }

    #[test]
    fn an_unpaired_sender_cannot_forge_a_packet() {
        let token = Token::random();
        let attacker = Token::random();
        let mut packet = [0u8; PACKET_LEN];
        encode_audio(&mut packet, MAGIC, &attacker, 0, 0, &[0i16; BLOCK_SAMPLES]);

        // It parses -- anyone can write a header -- but it does not verify.
        assert!(decode_audio(&packet, MAGIC).is_some());
        assert!(!verify_audio(&packet, &token));
    }

    #[test]
    fn tampering_with_the_payload_is_detected() {
        let token = Token::random();
        let mut packet = [0u8; PACKET_LEN];
        encode_audio(&mut packet, MAGIC, &token, 1, 2, &[7i16; BLOCK_SAMPLES]);
        assert!(verify_audio(&packet, &token));
        packet[HEADER_LEN + 10] ^= 0xFF;
        assert!(!verify_audio(&packet, &token));
    }

    #[test]
    fn garbage_is_rejected() {
        let token = Token::random();
        assert!(decode_audio(&[], MAGIC).is_none());
        assert!(decode_audio(&[0u8; PACKET_LEN], MAGIC).is_none());
        assert!(decode_audio(b"APU1 short", MAGIC).is_none());

        // A main-stream packet must not be accepted on the mic stream.
        let mut packet = [0u8; PACKET_LEN];
        encode_audio(&mut packet, MAGIC, &token, 0, 0, &[0i16; BLOCK_SAMPLES]);
        assert!(decode_audio(&packet, *b"XXXX").is_none());

        assert!(decode_discovery(b"nope").is_none());
        assert!(decode_discovery(&[b'A', b'P', b'D', b'1', 99]).is_none());
    }

    #[test]
    fn discovery_round_trips() {
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

    #[test]
    fn pairing_messages_round_trip() {
        let nonce = [9u8; 16];
        let proof = [3u8; 32];
        assert_eq!(
            decode_discovery(&encode_pair_request(&nonce, &proof)),
            Some(DiscoveryMessage::PairRequest { nonce, proof })
        );

        let token = Token::random();
        let accept = encode_pair_accept(&token, "LAPTOP", &proof);
        match decode_discovery(&accept) {
            Some(DiscoveryMessage::PairAccept {
                token: got,
                proof: got_proof,
                hostname,
            }) => {
                assert_eq!(got, token);
                assert_eq!(got_proof, proof);
                assert_eq!(hostname, "LAPTOP");
            }
            other => panic!("expected a pair accept, got {other:?}"),
        }

        assert_eq!(
            decode_discovery(&encode_pair_reject(PairReject::TooManyAttempts)),
            Some(DiscoveryMessage::PairReject(PairReject::TooManyAttempts))
        );
    }
}
