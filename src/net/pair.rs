//! The pairing exchange.
//!
//! The receiver shows a six-digit PIN. The sender proves it knows that PIN
//! and is handed a random 256-bit token, which both ends then use to sign
//! every audio packet.
//!
//! Six digits is only a million combinations, which is nothing to a machine
//! trying them over UDP. What makes it safe is [`PairingHost`]: a handful of
//! wrong guesses and the PIN is thrown away and a new one issued, so an
//! attacker never gets more than a few tries at any one secret.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::DISCOVERY_PORT;
use crate::net::protocol::{
    DISCOVERY_MAX, DiscoveryMessage, PairReject, decode_discovery, encode_pair_accept,
    encode_pair_reject, encode_pair_request,
};
use crate::pairing::{Pin, Token, random_nonce};

/// Wrong guesses tolerated before the receiver stops answering at all.
const MAX_ATTEMPTS: u32 = 5;
/// Minimum gap between attempts, so the five cannot be spent in a burst.
/// Zero under test, so the lockout tests do not have to sleep.
const MIN_ATTEMPT_GAP: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_millis(500)
};
/// How long a pairing window stays open once the user opens one.
const WINDOW: Duration = Duration::from_secs(120);
/// First lockout after the attempts are spent; it doubles from there.
const FIRST_LOCKOUT: Duration = Duration::from_secs(30);
const MAX_LOCKOUT: Duration = Duration::from_secs(900);
/// Nonces remembered, to refuse a replayed request.
const NONCE_MEMORY: usize = 64;
/// How long the sender waits for an answer before giving up.
const PAIR_TIMEOUT: Duration = Duration::from_millis(2500);

/// Receiver side: issues the PIN and hands out tokens.
///
/// Held by the GUI so the PIN can be displayed, and consulted by the
/// announcer thread when a pairing request arrives.
pub struct PairingHost {
    state: Mutex<HostState>,
}

struct HostState {
    pin: Pin,
    attempts: u32,
    /// Pairing is refused entirely outside this window. Nothing listens for
    /// a PIN until the user opens one, which is what keeps six digits safe.
    window_until: Option<Instant>,
    /// Set when the attempts are spent. Doubling, so repeated guessing gets
    /// slower and slower rather than resetting.
    locked_until: Option<Instant>,
    lockout: Duration,
    last_attempt: Option<Instant>,
    /// Recently seen nonces, so a captured request cannot be replayed.
    recent_nonces: std::collections::VecDeque<[u8; 16]>,
}

/// What a pairing attempt produced, for the UI to report.
pub enum PairOutcome {
    /// A new sender was paired; the token must be stored and trusted.
    Paired { token: Token },
    Refused(PairReject),
}

impl PairingHost {
    pub fn new() -> PairingHost {
        PairingHost {
            state: Mutex::new(HostState {
                pin: Pin::random(),
                attempts: 0,
                window_until: None,
                locked_until: None,
                lockout: FIRST_LOCKOUT,
                last_attempt: None,
                recent_nonces: std::collections::VecDeque::with_capacity(NONCE_MEMORY),
            }),
        }
    }

    /// Opens a pairing window: a fresh PIN, answered for two minutes.
    ///
    /// Everything about this design rests on the window. Six digits is a
    /// million combinations, which a machine guesses in seconds if it is
    /// allowed to keep trying; it is safe only because almost no guesses are
    /// accepted. Rotating the PIN on failure is *not* a substitute, because
    /// an attacker does not care which PIN they hit -- it leaves the expected
    /// work at one in a million either way.
    pub fn open_window(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.pin = Pin::random();
            state.attempts = 0;
            state.window_until = Some(Instant::now() + WINDOW);
            state.locked_until = None;
            state.lockout = FIRST_LOCKOUT;
            state.last_attempt = None;
        }
    }

    pub fn close_window(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.window_until = None;
        }
    }

    /// The PIN to show, or `None` when no window is open.
    pub fn display_pin(&self) -> Option<String> {
        let state = self.state.lock().ok()?;
        let until = state.window_until?;
        (Instant::now() < until).then(|| state.pin.display())
    }

    /// Seconds left in the open window, for the countdown on screen.
    pub fn window_remaining(&self) -> Option<Duration> {
        let state = self.state.lock().ok()?;
        let until = state.window_until?;
        until.checked_duration_since(Instant::now())
    }

    /// How long pairing stays refused after too many wrong guesses.
    pub fn locked_for(&self) -> Option<Duration> {
        let state = self.state.lock().ok()?;
        state
            .locked_until
            .and_then(|until| until.checked_duration_since(Instant::now()))
    }

    /// Judges one pairing request. Called from the announcer thread.
    pub fn handle_request(&self, nonce: &[u8; 16], proof: &[u8; 32]) -> PairOutcome {
        let Ok(mut state) = self.state.lock() else {
            return PairOutcome::Refused(PairReject::WrongPin);
        };
        let now = Instant::now();

        // Outside an open window nothing is answered at all.
        match state.window_until {
            Some(until) if now < until => {}
            _ => return PairOutcome::Refused(PairReject::NotOpen),
        }

        // A spent window stays locked, for longer each time.
        if let Some(until) = state.locked_until {
            if now < until {
                return PairOutcome::Refused(PairReject::TooManyAttempts);
            }
            state.locked_until = None;
            state.attempts = 0;
        }

        // Attempts cannot be spent in a burst.
        if let Some(last) = state.last_attempt {
            if now.duration_since(last) < MIN_ATTEMPT_GAP {
                return PairOutcome::Refused(PairReject::TooManyAttempts);
            }
        }
        state.last_attempt = Some(now);

        // A replayed request must not consume an attempt or succeed twice.
        if state.recent_nonces.contains(nonce) {
            return PairOutcome::Refused(PairReject::WrongPin);
        }
        if state.recent_nonces.len() == NONCE_MEMORY {
            state.recent_nonces.pop_front();
        }
        state.recent_nonces.push_back(*nonce);

        if !state.pin.verify_proof(nonce, proof) {
            state.attempts += 1;
            if state.attempts >= MAX_ATTEMPTS {
                state.locked_until = Some(now + state.lockout);
                state.lockout = (state.lockout * 2).min(MAX_LOCKOUT);
                return PairOutcome::Refused(PairReject::TooManyAttempts);
            }
            return PairOutcome::Refused(PairReject::WrongPin);
        }

        // Success closes the window and burns the PIN, so one PIN pairs one
        // device. Anyone who overheard it has nothing left to use.
        state.attempts = 0;
        state.window_until = None;
        state.pin = Pin::random();
        PairOutcome::Paired {
            token: Token::random(),
        }
    }

    /// Builds the reply to send back. Separated from [`Self::handle_request`]
    /// so the socket code stays in one place.
    pub fn reply(&self, outcome: &PairOutcome, nonce: &[u8; 16], hostname: &str) -> Vec<u8> {
        match outcome {
            PairOutcome::Paired { token } => {
                // Proof that the receiver knew the PIN too, so a machine
                // impersonating the receiver cannot hand out a token of its
                // own choosing.
                let Ok(state) = self.state.lock() else {
                    return encode_pair_reject(PairReject::WrongPin);
                };
                let mut context = Vec::with_capacity(16 + 32);
                context.extend_from_slice(nonce);
                context.extend_from_slice(token.as_bytes());
                let proof = state.pin.proof(&context);
                encode_pair_accept(token, hostname, &proof)
            }
            PairOutcome::Refused(reason) => encode_pair_reject(*reason),
        }
    }
}

/// Sender side: proves knowledge of the PIN and collects the token.
///
/// Blocking, and quick: one datagram out, one back, with a short timeout. Run
/// it off the GUI thread.
pub fn pair_with(receiver_ip: std::net::IpAddr, pin: &Pin) -> Result<(Token, String), String> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| format!("no socket: {e}"))?;
    socket
        .set_read_timeout(Some(PAIR_TIMEOUT))
        .map_err(|e| e.to_string())?;

    let nonce = random_nonce();
    let request = encode_pair_request(&nonce, &pin.proof(&nonce));
    socket
        .send_to(&request, SocketAddr::new(receiver_ip, DISCOVERY_PORT))
        .map_err(|e| format!("could not reach the receiver: {e}"))?;

    let mut buf = [0u8; DISCOVERY_MAX];
    let deadline = Instant::now() + PAIR_TIMEOUT;
    while Instant::now() < deadline {
        let Ok((len, from)) = socket.recv_from(&mut buf) else {
            break;
        };
        if from.ip() != receiver_ip {
            continue;
        }
        match decode_discovery(&buf[..len]) {
            Some(DiscoveryMessage::PairAccept {
                token,
                proof,
                hostname,
            }) => {
                // Check the receiver knew the PIN as well. Without this, a
                // machine that simply answered first could plant a token and
                // then read everything we send.
                let mut context = Vec::with_capacity(16 + 32);
                context.extend_from_slice(&nonce);
                context.extend_from_slice(token.as_bytes());
                if !pin.verify_proof(&context, &proof) {
                    return Err(
                        "The reply did not come from the receiver showing that PIN.".to_string()
                    );
                }
                return Ok((token, hostname));
            }
            Some(DiscoveryMessage::PairReject(reason)) => {
                return Err(reason.message().to_string());
            }
            _ => continue,
        }
    }

    Err("The receiver did not answer. Is it started, and on this network?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::random_nonce;

    /// Reads the PIN back off the host, as a user would off the screen.
    fn shown_pin(host: &PairingHost) -> Pin {
        Pin::parse(&host.display_pin().expect("a window should be open")).unwrap()
    }

    fn attempt(host: &PairingHost, pin: &Pin) -> PairOutcome {
        let nonce = random_nonce();
        let proof = pin.proof(&nonce);
        host.handle_request(&nonce, &proof)
    }

    #[test]
    fn nothing_is_answered_until_a_window_is_opened() {
        let host = PairingHost::new();
        assert!(host.display_pin().is_none());
        let wrong = Pin::parse("000000").unwrap();
        assert!(matches!(
            attempt(&host, &wrong),
            PairOutcome::Refused(PairReject::NotOpen)
        ));
    }

    #[test]
    fn the_right_pin_pairs_once_and_then_the_window_closes() {
        let host = PairingHost::new();
        host.open_window();
        let pin = shown_pin(&host);

        assert!(matches!(attempt(&host, &pin), PairOutcome::Paired { .. }));

        // One PIN pairs one device: anyone who overheard it has nothing left.
        assert!(host.display_pin().is_none());
        assert!(matches!(
            attempt(&host, &pin),
            PairOutcome::Refused(PairReject::NotOpen)
        ));
    }

    #[test]
    fn guessing_locks_out_instead_of_rotating_forever() {
        let host = PairingHost::new();
        host.open_window();
        let real = shown_pin(&host);
        // A wrong PIN that is definitely not the real one.
        let wrong = if real.as_str() == "000000" {
            Pin::parse("111111").unwrap()
        } else {
            Pin::parse("000000").unwrap()
        };

        for _ in 0..MAX_ATTEMPTS {
            assert!(matches!(attempt(&host, &wrong), PairOutcome::Refused(_)));
        }

        // Locked out, and -- the point of the fix -- even the correct PIN is
        // refused while locked. Rotation alone would have let an attacker
        // keep guessing at one-in-a-million odds indefinitely.
        assert!(host.locked_for().is_some());
        assert!(matches!(
            attempt(&host, &real),
            PairOutcome::Refused(PairReject::TooManyAttempts)
        ));
    }

    #[test]
    fn a_replayed_request_is_refused() {
        let host = PairingHost::new();
        host.open_window();
        let pin = shown_pin(&host);
        let nonce = random_nonce();
        let proof = pin.proof(&nonce);

        assert!(matches!(
            host.handle_request(&nonce, &proof),
            PairOutcome::Paired { .. }
        ));

        // Replaying the captured request must not pair a second machine.
        host.open_window();
        assert!(matches!(
            host.handle_request(&nonce, &proof),
            PairOutcome::Refused(_)
        ));
    }
}
