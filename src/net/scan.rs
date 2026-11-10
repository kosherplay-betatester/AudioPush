//! Network scan: the fallback when broadcast discovery is blocked.
//!
//! Rather than guess which hosts exist, we send a tiny probe to every address
//! in the local subnet(s) on the discovery port. Any machine running
//! AudioPush in receiver mode answers immediately with its normal
//! announcement, which the discovery listener already knows how to handle.
//! Machines that are not running AudioPush simply ignore it.

use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::config::DISCOVERY_PORT;
use crate::net::local_addresses;
use crate::net::protocol::encode_probe;

/// Upper bound on addresses probed in one scan, so an unusually large subnet
/// cannot turn the button into a multi-minute operation.
const MAX_TARGETS: usize = 2048;
/// Probes sent before pausing, to stay friendly to the ARP cache.
const BATCH: usize = 64;

/// Progress and completion flags the GUI polls while a scan runs.
#[derive(Default)]
pub struct ScanProgress {
    pub sent: AtomicU32,
    pub total: AtomicU32,
    pub running: AtomicBool,
}

/// Fires probes at every host on the local subnets. Replies arrive on
/// `socket`, which the discovery listener is already reading.
pub fn spawn(socket: Arc<UdpSocket>, progress: Arc<ScanProgress>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("audiopush-scan".into())
        .spawn(move || {
            progress.running.store(true, Ordering::Relaxed);
            progress.sent.store(0, Ordering::Relaxed);

            let mut targets: Vec<SocketAddr> = Vec::new();
            for local in local_addresses() {
                // /8 and the like would be millions of addresses; skip them.
                if local.subnet_size() > MAX_TARGETS as u64 * 2 {
                    continue;
                }
                for host in local.hosts() {
                    if host == local.ip {
                        continue;
                    }
                    targets.push(SocketAddr::V4(SocketAddrV4::new(host, DISCOVERY_PORT)));
                    if targets.len() >= MAX_TARGETS {
                        break;
                    }
                }
                if targets.len() >= MAX_TARGETS {
                    break;
                }
            }

            progress.total.store(targets.len() as u32, Ordering::Relaxed);
            let probe = encode_probe();

            for (index, target) in targets.iter().enumerate() {
                let _ = socket.send_to(&probe, target);
                progress.sent.store(index as u32 + 1, Ordering::Relaxed);
                if index % BATCH == BATCH - 1 {
                    thread::sleep(Duration::from_millis(2));
                }
            }

            // Give slow repliers a moment before the GUI declares the scan done.
            thread::sleep(Duration::from_millis(600));
            progress.running.store(false, Ordering::Relaxed);
        })
        .expect("failed to spawn scan thread")
}
