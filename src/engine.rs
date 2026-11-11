//! Starts and stops the worker threads for one streaming session.
//!
//! A session owns its rings and its threads. Stopping sets one atomic flag
//! and joins; every worker loop checks that flag on a timeout, so shutdown is
//! prompt and nothing is left running behind the GUI.
//!
//! Two streams can be in flight at once, in opposite directions:
//!
//! ```text
//!   workstation (sender)                       laptop / phone (receiver)
//!   loopback capture  --- UDP 45778 "APU1" -->  jitter buffer -> speakers
//!   chosen output     <-- UDP 45779 "APM1" ---  microphone capture
//! ```
//!
//! The return stream is optional and is switched on from the receiver, which
//! is the machine whose microphone would be live.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::audio::capture::{self, Source};
use crate::audio::render;
use crate::config::{Latency, MIC_PORT, RING_BLOCKS};
use crate::net::discovery::PairingQueue;
use crate::net::pair::PairingHost;
use crate::net::protocol::{MAGIC, MIC_MAGIC};
use crate::net::{discovery, receiver, sender};
use crate::pairing::{Token, TokenStore, new_token_slot};
use crate::ring::BlockRing;
use crate::state::Shared;

/// The return microphone stream, as configured in the GUI.
#[derive(Clone, Debug, Default)]
pub struct MicConfig {
    pub enabled: bool,
    /// On the receiver, the microphone to capture from.
    /// On the sender, the output endpoint to play the microphone into.
    pub device_id: Option<String>,
}

pub struct Session {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl Session {
    /// Workstation side: loopback capture out, microphone in.
    pub fn start_sender(
        device_id: Option<String>,
        preset: Latency,
        dest: SocketAddr,
        token: Token,
        shared: Arc<Shared>,
        mic: MicConfig,
        mic_shared: Arc<Shared>,
    ) -> Session {
        shared.reset_counters();
        let stop = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(BlockRing::new(RING_BLOCKS));

        let mut handles = vec![
            capture::spawn(
                device_id,
                Source::SystemAudio,
                preset,
                Arc::clone(&ring),
                Arc::clone(&shared),
                Arc::clone(&stop),
            ),
            sender::spawn(
                sender::new_target(Some(dest)),
                new_token_slot(Some(token.clone())),
                MAGIC,
                Arc::clone(&ring),
                Arc::clone(&shared),
                Arc::clone(&stop),
            ),
        ];

        if mic.enabled {
            mic_shared.reset_counters();
            let mic_ring = Arc::new(BlockRing::new(RING_BLOCKS));
            handles.push(receiver::spawn(
                MIC_PORT,
                MIC_MAGIC,
                // Only the receiver we paired with may send us a
                // microphone, so exactly one token is trusted here.
                Arc::new(TokenStore::new(vec![token])),
                Arc::clone(&mic_ring),
                Arc::clone(&mic_shared),
                Arc::clone(&stop),
                None,
                None,
            ));
            handles.push(render::spawn(
                mic.device_id,
                preset,
                mic_ring,
                mic_shared,
                Arc::clone(&stop),
            ));
        }

        Session { stop, handles }
    }

    /// Laptop or phone side: audio in, microphone out.
    /// Also announces this machine so senders can find it by name.
    pub fn start_receiver(
        device_id: Option<String>,
        preset: Latency,
        port: u16,
        tokens: Arc<TokenStore>,
        host: Arc<PairingHost>,
        queue: PairingQueue,
        shared: Arc<Shared>,
        mic: MicConfig,
        mic_shared: Arc<Shared>,
    ) -> Session {
        shared.reset_counters();
        let stop = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(BlockRing::new(RING_BLOCKS));

        // Where the microphone stream should go. Empty until the first audio
        // packet arrives and tells us who is streaming to us.
        let mic_target = sender::new_target(None);
        // Which pairing to sign the microphone with is decided by
        // whichever sender authenticates first.
        let mic_token = new_token_slot(None);

        let mut handles = vec![
            receiver::spawn(
                port,
                MAGIC,
                Arc::clone(&tokens),
                Arc::clone(&ring),
                Arc::clone(&shared),
                Arc::clone(&stop),
                mic.enabled.then(|| Arc::clone(&mic_target)),
                mic.enabled.then(|| Arc::clone(&mic_token)),
            ),
            render::spawn(
                device_id,
                preset,
                Arc::clone(&ring),
                Arc::clone(&shared),
                Arc::clone(&stop),
            ),
        ];

        if mic.enabled {
            mic_shared.reset_counters();
            let mic_ring = Arc::new(BlockRing::new(RING_BLOCKS));
            handles.push(capture::spawn(
                mic.device_id,
                Source::Microphone,
                preset,
                Arc::clone(&mic_ring),
                Arc::clone(&mic_shared),
                Arc::clone(&stop),
            ));
            handles.push(sender::spawn(
                mic_target,
                mic_token,
                MIC_MAGIC,
                mic_ring,
                mic_shared,
                Arc::clone(&stop),
            ));
        }

        match discovery::start_announcer(port, Arc::clone(&stop), host, tokens, queue) {
            Ok(handle) => handles.push(handle),
            Err(err) => {
                // Discovery is a convenience; streaming still works if the
                // sender is given the address by hand.
                shared.set_net_error(Some(format!("{err} Manual IP entry still works.")));
            }
        }

        Session { stop, handles }
    }

    /// Signals every worker to finish and waits for them.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}
