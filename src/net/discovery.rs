//! Automatic peer discovery.
//!
//! The receiver announces itself once a second by UDP broadcast, on every
//! interface it has. The sender listens on the same port and keeps a live
//! list, so the user picks a machine by name instead of typing an address.
//!
//! The same socket also answers scan probes (see `scan.rs`), which covers
//! networks where broadcast traffic is filtered.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::{ANNOUNCE_INTERVAL_MS, DISCOVERY_PORT, PEER_TIMEOUT_MS};
use crate::net::protocol::{
    DISCOVERY_MAX, DiscoveryMessage, decode_discovery, encode_announce,
};
use crate::net::{hostname, local_addresses};
use crate::state::{Peer, now_ms};

/// Shared, GUI-readable list of receivers we have heard from.
pub type PeerList = Arc<Mutex<Vec<Peer>>>;

pub fn new_peer_list() -> PeerList {
    Arc::new(Mutex::new(Vec::new()))
}

fn bind_discovery() -> Result<UdpSocket, String> {
    let socket = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)).map_err(|e| {
        format!(
            "could not use discovery port {DISCOVERY_PORT}: {e}. \
             Another copy of AudioPush may already be running on this PC."
        )
    })?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| e.to_string())?;
    let _ = socket.set_broadcast(true);
    Ok(socket)
}

// ---------------------------------------------------------------------------
// Receiver side
// ---------------------------------------------------------------------------

/// Starts announcing this machine as an available receiver.
pub fn start_announcer(audio_port: u16, stop: Arc<AtomicBool>) -> Result<JoinHandle<()>, String> {
    let socket = bind_discovery()?;
    let name = hostname();

    let handle = thread::Builder::new()
        .name("audiopush-announce".into())
        .spawn(move || {
            let payload = encode_announce(&name, audio_port);
            let mut buf = [0u8; DISCOVERY_MAX];
            // Send one immediately so a waiting sender sees us at once.
            let mut last_announce = Instant::now() - Duration::from_secs(10);

            while !stop.load(Ordering::Relaxed) {
                if last_announce.elapsed() >= Duration::from_millis(ANNOUNCE_INTERVAL_MS) {
                    last_announce = Instant::now();
                    broadcast(&socket, &payload);
                }

                // Answer scan probes straight away, so a scan feels instant.
                if let Ok((len, from)) = socket.recv_from(&mut buf) {
                    if let Some(DiscoveryMessage::Probe) = decode_discovery(&buf[..len]) {
                        let _ = socket.send_to(&payload, from);
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;

    Ok(handle)
}

/// Sends an announcement to every subnet broadcast address, plus the
/// all-hosts address as a fallback for odd network setups.
fn broadcast(socket: &UdpSocket, payload: &[u8]) {
    for local in local_addresses() {
        let target = SocketAddr::V4(SocketAddrV4::new(local.broadcast(), DISCOVERY_PORT));
        let _ = socket.send_to(payload, target);
    }
    let all_hosts = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT));
    let _ = socket.send_to(payload, all_hosts);
}

// ---------------------------------------------------------------------------
// Sender side
// ---------------------------------------------------------------------------

/// Starts listening for receiver announcements.
///
/// Returns the socket as well, so a network scan can send its probes from the
/// same port and have the replies arrive back here.
pub fn start_listener(
    peers: PeerList,
    stop: Arc<AtomicBool>,
) -> Result<(Arc<UdpSocket>, JoinHandle<()>), String> {
    let socket = Arc::new(bind_discovery()?);
    let thread_socket = Arc::clone(&socket);
    let own_ips: Vec<Ipv4Addr> = local_addresses().into_iter().map(|a| a.ip).collect();

    let handle = thread::Builder::new()
        .name("audiopush-discover".into())
        .spawn(move || {
            let mut buf = [0u8; DISCOVERY_MAX];
            while !stop.load(Ordering::Relaxed) {
                if let Ok((len, from)) = thread_socket.recv_from(&mut buf) {
                    if let Some(DiscoveryMessage::Announce {
                        hostname,
                        audio_port,
                    }) = decode_discovery(&buf[..len])
                    {
                        if let SocketAddr::V4(v4) = from {
                            // Our own broadcast comes back to us; skip it.
                            if own_ips.contains(v4.ip()) {
                                continue;
                            }
                            let addr =
                                SocketAddr::V4(SocketAddrV4::new(*v4.ip(), audio_port));
                            upsert(&peers, hostname, addr);
                        }
                    }
                }
                prune(&peers);
            }
        })
        .map_err(|e| e.to_string())?;

    Ok((socket, handle))
}

/// Adds a newly heard receiver, or refreshes one we already know.
fn upsert(peers: &PeerList, hostname: String, addr: SocketAddr) {
    let Ok(mut list) = peers.lock() else { return };
    let now = now_ms();
    if let Some(existing) = list.iter_mut().find(|p| p.addr.ip() == addr.ip()) {
        existing.hostname = hostname;
        existing.addr = addr;
        existing.last_seen_ms = now;
    } else {
        list.push(Peer {
            hostname,
            addr,
            last_seen_ms: now,
        });
        list.sort_by(|a, b| a.hostname.cmp(&b.hostname));
    }
}

/// Forgets receivers that stopped announcing.
fn prune(peers: &PeerList) {
    let Ok(mut list) = peers.lock() else { return };
    let now = now_ms();
    list.retain(|p| now.saturating_sub(p.last_seen_ms) < PEER_TIMEOUT_MS);
}
