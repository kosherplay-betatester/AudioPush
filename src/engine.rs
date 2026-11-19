//! Starts and stops the worker threads for one streaming session.
//!
//! A session owns its rings and its threads. Stopping sets one atomic flag
//! and joins; every worker loop checks that flag on a timeout, so shutdown is
//! prompt and nothing is left running behind the GUI.
//!
//! One stream, one direction:
//!
//! ```text
//!   workstation (sender)                       laptop / phone (receiver)
//!   loopback capture  --- UDP 45778 "APU1" -->  jitter buffer -> speakers
//! ```

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::audio::capture;
use crate::audio::render;
use crate::config::{Latency, RING_BLOCKS};
use crate::net::discovery::PairingQueue;
use crate::net::pair::PairingHost;
use crate::net::protocol::MAGIC;
use crate::net::{discovery, receiver, sender};
use crate::pairing::{Token, TokenStore, new_token_slot};
use crate::ring::BlockRing;
use crate::state::Shared;

pub struct Session {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl Session {
    /// Workstation side: loopback capture out.
    pub fn start_sender(
        device_id: Option<String>,
        preset: Latency,
        dest: SocketAddr,
        token: Token,
        shared: Arc<Shared>,
    ) -> Session {
        shared.reset_counters();
        let stop = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(BlockRing::new(RING_BLOCKS));

        let handles = vec![
            capture::spawn(
                device_id,
                preset,
                Arc::clone(&ring),
                Arc::clone(&shared),
                Arc::clone(&stop),
            ),
            sender::spawn(
                sender::new_target(Some(dest)),
                new_token_slot(Some(token)),
                MAGIC,
                Arc::clone(&ring),
                Arc::clone(&shared),
                Arc::clone(&stop),
            ),
        ];

        Session { stop, handles }
    }

    /// Laptop or phone side: audio in.
    /// Also announces this machine so senders can find it by name.
    pub fn start_receiver(
        device_id: Option<String>,
        preset: Latency,
        port: u16,
        tokens: Arc<TokenStore>,
        host: Arc<PairingHost>,
        queue: PairingQueue,
        shared: Arc<Shared>,
    ) -> Session {
        shared.reset_counters();
        let stop = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(BlockRing::new(RING_BLOCKS));

        let mut handles = vec![
            receiver::spawn(
                port,
                MAGIC,
                Arc::clone(&tokens),
                Arc::clone(&ring),
                Arc::clone(&shared),
                Arc::clone(&stop),
            ),
            render::spawn(
                device_id,
                preset,
                Arc::clone(&ring),
                Arc::clone(&shared),
                Arc::clone(&stop),
            ),
        ];

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
