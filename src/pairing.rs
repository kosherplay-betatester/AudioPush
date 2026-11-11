//! Pairing secrets and packet authentication.
//!
//! The model, in one paragraph: a receiver shows a six-digit PIN. The sender
//! proves it knows that PIN, and in return is given a random 256-bit token.
//! From then on every audio and microphone packet carries a truncated
//! HMAC-SHA256 tag computed with that token, so an unpaired machine on the
//! network can neither play audio through your speakers nor redirect your
//! microphone to itself.
//!
//! What this protects against: anyone who was not listening to the network at
//! the exact moment of pairing. That is the realistic threat on a home or
//! office WiFi, where the attacker is a guest or neighbour who already has
//! the WiFi password.
//!
//! What it does not protect against: someone capturing the pairing exchange
//! itself, since the token crosses the wire once in the clear. Closing that
//! would need a password-authenticated key exchange such as SPAKE2, which is
//! a great deal of cryptography to get wrong for a three-second window. The
//! README says so plainly.

use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use rand::RngExt;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Length of the authentication tag carried by every audio packet.
/// Eight bytes is 64 bits of forgery resistance, which is ample when a
/// forged packet buys an attacker 5 ms of audio.
pub const TAG_LEN: usize = 8;

/// A pairing secret shared by one sender and one receiver.
#[derive(Clone, PartialEq, Eq)]
pub struct Token([u8; 32]);

impl Token {
    pub fn random() -> Token {
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes[..]);
        Token(bytes)
    }

    pub fn from_hex(text: &str) -> Option<Token> {
        if text.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(Token(bytes))
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Token> {
        let mut out = [0u8; 32];
        if bytes.len() != 32 {
            return None;
        }
        out.copy_from_slice(bytes);
        Some(Token(out))
    }

    /// Short, non-secret identifier carried in each packet so the receiver
    /// can pick the right token in one step instead of trying all of them.
    pub fn key_id(&self) -> u32 {
        let digest = Sha256::digest(self.0);
        u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]])
    }

    /// Authentication tag for one packet.
    pub fn tag(&self, data: &[u8]) -> [u8; TAG_LEN] {
        let mut mac = HmacSha256::new_from_slice(&self.0).expect("HMAC accepts any key length");
        mac.update(data);
        let full = mac.finalize().into_bytes();
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&full[..TAG_LEN]);
        tag
    }

    /// Constant-time check of a packet's tag.
    pub fn verify(&self, data: &[u8], tag: &[u8]) -> bool {
        if tag.len() != TAG_LEN {
            return false;
        }
        constant_time_eq(&self.tag(data), tag)
    }
}

impl std::fmt::Debug for Token {
    /// Never print the secret, not even by accident in a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Token({:08x})", self.key_id())
    }
}

/// A six-digit pairing PIN, held only in memory and only while pairing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pin(String);

impl Pin {
    pub fn random() -> Pin {
        let mut bytes = [0u8; 4];
        rand::rng().fill(&mut bytes[..]);
        // Six digits: 10^6 combinations. On its own that is weak, which is
        // why the receiver allows only a handful of attempts before issuing
        // a new one -- see `PairingHost`.
        let value = u32::from_le_bytes(bytes) % 1_000_000;
        Pin(format!("{value:06}"))
    }

    pub fn parse(text: &str) -> Option<Pin> {
        let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() == 6 {
            Some(Pin(digits))
        } else {
            None
        }
    }

    /// Grouped for reading aloud across a room.
    pub fn display(&self) -> String {
        format!("{} {}", &self.0[..3], &self.0[3..])
    }

    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Proof that both ends know the PIN, over whatever context they agree on.
    pub fn proof(&self, context: &[u8]) -> [u8; 32] {
        let mut mac =
            HmacSha256::new_from_slice(self.0.as_bytes()).expect("HMAC accepts any key length");
        mac.update(context);
        let bytes = mac.finalize().into_bytes();
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    }

    pub fn verify_proof(&self, context: &[u8], proof: &[u8]) -> bool {
        proof.len() == 32 && constant_time_eq(&self.proof(context), proof)
    }
}

/// The tokens a machine will accept packets signed with.
///
/// A receiver paired with several senders holds one per sender; a sender
/// holds the single token of the receiver it is streaming to. Lookup is by
/// `key_id`, so verifying a packet costs one HMAC rather than one per token.
pub struct TokenStore {
    tokens: std::sync::Mutex<Vec<Token>>,
}

impl TokenStore {
    pub fn new(tokens: Vec<Token>) -> TokenStore {
        TokenStore {
            tokens: std::sync::Mutex::new(tokens),
        }
    }

    pub fn find(&self, key_id: u32) -> Option<Token> {
        let guard = self.tokens.lock().ok()?;
        guard.iter().find(|t| t.key_id() == key_id).cloned()
    }

    /// Adds a token the moment a new sender pairs, so it can start streaming
    /// without the receiver being restarted.
    pub fn add(&self, token: Token) {
        if let Ok(mut guard) = self.tokens.lock() {
            if !guard.iter().any(|t| t == &token) {
                guard.push(token);
            }
        }
    }
}

/// A token that is not known until a peer appears: the microphone stream
/// learns which pairing to sign with when the first audio packet arrives.
pub type TokenSlot = std::sync::Arc<std::sync::Mutex<Option<Token>>>;

pub fn new_token_slot(token: Option<Token>) -> TokenSlot {
    std::sync::Arc::new(std::sync::Mutex::new(token))
}

/// Comparison that does not leak how many bytes matched.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Fills a buffer with cryptographic randomness.
pub fn random_nonce() -> [u8; 16] {
    let mut nonce = [0u8; 16];
    rand::rng().fill(&mut nonce[..]);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_round_trips_and_rejects_tampering() {
        let token = Token::random();
        let data = b"the quick brown fox";
        let tag = token.tag(data);
        assert!(token.verify(data, &tag));
        assert!(!token.verify(b"the quick brown fix", &tag));

        let other = Token::random();
        assert!(!other.verify(data, &tag));
    }

    #[test]
    fn token_hex_round_trips() {
        let token = Token::random();
        let hex = token.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(Token::from_hex(&hex), Some(token));
        assert_eq!(Token::from_hex("nonsense"), None);
    }

    #[test]
    fn pins_are_six_digits_and_proofs_are_checked() {
        let pin = Pin::random();
        assert_eq!(pin.as_str().len(), 6);
        assert!(pin.as_str().chars().all(|c| c.is_ascii_digit()));
        assert_eq!(pin.display().len(), 7);

        let context = b"nonce and name";
        let proof = pin.proof(context);
        assert!(pin.verify_proof(context, &proof));
        assert!(!pin.verify_proof(b"different context", &proof));

        let other = Pin::parse("000000").unwrap();
        assert!(!other.verify_proof(context, &proof) || other == pin);
    }

    #[test]
    fn pin_parsing_is_strict() {
        assert_eq!(Pin::parse("123 456").map(|p| p.as_str().to_string()), Some("123456".into()));
        assert!(Pin::parse("12345").is_none());
        assert!(Pin::parse("1234567").is_none());
    }

    #[test]
    fn key_id_is_stable_and_distinguishes_tokens() {
        let token = Token::random();
        assert_eq!(token.key_id(), token.key_id());
        assert_ne!(token.key_id(), Token::random().key_id());
    }
}
