//! Starts and stops the worker threads for one streaming session.
//!
//! A session owns its ring and its threads. Stopping sets one atomic flag and
//! joins; every worker loop checks that flag on a timeout, so shutdown is
//! prompt and nothing is left running behind the GUI.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crate::audio::{capture, render};
use crate::config::{Latency, RING_BLOCKS};
use crate::net::{discovery, receiver, sender};
use crate::ring::BlockRing;
use crate::state::Shared;

pub struct Session {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl Session {
    /// Workstation side: loopback capture into UDP.
    pub fn start_sender(
        device_id: Option<String>,
        preset: Latency,
        dest: SocketAddr,
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
                dest,
                Arc::clone(&ring),
                Arc::clone(&shared),
                Arc::clone(&stop),
            ),
        ];

        Session { stop, handles }
    }

    /// Laptop side: UDP into the jitter buffer and out of the speakers.
    /// Also announces this machine so senders can find it by name.
    pub fn start_receiver(
        device_id: Option<String>,
        preset: Latency,
        port: u16,
        shared: Arc<Shared>,
    ) -> Session {
        shared.reset_counters();
        let stop = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(BlockRing::new(RING_BLOCKS));

        let mut handles = vec![
            receiver::spawn(
                port,
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

        match discovery::start_announcer(port, Arc::clone(&stop)) {
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
