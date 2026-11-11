//! Peer-to-peer party over iroh (QUIC, NAT traversal, relay fallback). No central server and
//! no port forwarding: peers dial each other by public key.
//!
//! Topology: a star around the host. Every peer keeps the full event log (a set merged by
//! union). On every (re)connect both sides exchange `Have{ids}` and send each other what is
//! missing, so reconnects lose nothing. If the host leaves or disappears, the next member in
//! roster order becomes host and everyone else re-dials it.

mod actor;
pub mod proto;

pub use actor::{spawn, LinkState, NetCmd, NetConfig, NetEvent, NetHandle, PartyState};
pub use proto::{make_code, parse_code, RosterMember};

/// The player id (and party address) that a secret key maps to. No networking involved.
pub fn identity_from_secret(secret: &[u8; 32]) -> String {
    proto::id_to_string(iroh::SecretKey::from_bytes(secret).public().as_bytes())
}
