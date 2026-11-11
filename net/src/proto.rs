//! Wire protocol, party codes and handoff rules. Pure (no iroh), unit-tested.

use lootline_core::Event;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const ALPN: &[u8] = b"lootline/party/1";
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
pub const EVENTS_PER_MSG: usize = 400;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RosterMember {
    pub id: String,
    pub connected: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Msg {
    /// "These are the event ids I already have": the peer replies with everything else.
    Have {
        ids: Vec<Uuid>,
    },
    Events {
        events: Vec<Event>,
    },
    /// Host -> everyone: who is in the party (host first, then join order) and who is connected.
    Roster {
        host: String,
        members: Vec<RosterMember>,
    },
    /// Host -> peer: you have been removed from the party.
    Kicked,
    /// Host -> everyone: the host is leaving; `next` becomes the new host.
    HostLeaving {
        next: Option<String>,
    },
}

pub fn encode_frame(m: &Msg) -> Vec<u8> {
    let body = serde_json::to_vec(m).expect("Msg serializes");
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn decode_body(body: &[u8]) -> Result<Msg, serde_json::Error> {
    serde_json::from_slice(body)
}

/// Split events into messages of bounded size.
pub fn event_msgs(events: Vec<Event>) -> Vec<Msg> {
    events.chunks(EVENTS_PER_MSG).map(|c| Msg::Events { events: c.to_vec() }).collect()
}

// ------------------------------------------------------------------ party codes

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// RFC 4648 base32 (lowercase, no padding) of 32 bytes -> 52 characters.
pub fn id_to_string(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(52);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn string_to_id(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    let (mut buf, mut bits, mut n) = (0u32, 0u32, 0usize);
    for c in s.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c.to_ascii_lowercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            if n >= 32 {
                return None;
            }
            out[n] = ((buf >> bits) & 0xff) as u8;
            n += 1;
        }
    }
    (n == 32).then_some(out)
}

/// Human-friendly party code: `LL-xxxx-xxxx-...` (13 groups of 4).
pub fn make_code(id: &str) -> String {
    let groups: Vec<&str> = id.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
    format!("LL-{}", groups.join("-"))
}

/// Accepts the code with or without dashes/spaces/prefix, in any case.
pub fn parse_code(code: &str) -> Option<String> {
    let t: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let candidate = match t.len() {
        52 => t.clone(),
        54 if t.starts_with("ll") => t[2..].to_string(),
        _ => return None,
    };
    string_to_id(&candidate).map(|_| candidate)
}

/// Who becomes host when the current host disappears: the first remaining member in roster order.
pub fn next_host(members: &[String], old_host: &str) -> Option<String> {
    members.iter().find(|m| m.as_str() != old_host).cloned()
}

/// A member dialed us while we, too, have lost the host: should we become host right away?
/// True when the handoff rule (first live member other than the old host) picks us.
pub fn should_take_over(
    members: &[String],
    dead: &std::collections::HashSet<String>,
    old_host: &str,
    me: &str,
) -> bool {
    let alive: Vec<String> = members.iter().filter(|m| !dead.contains(*m)).cloned().collect();
    next_host(&alive, old_host).as_deref() == Some(me)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_roundtrip() {
        let mut b = [0u8; 32];
        for (i, x) in b.iter_mut().enumerate() {
            *x = (i * 37 + 11) as u8;
        }
        let s = id_to_string(&b);
        assert_eq!(s.len(), 52);
        assert_eq!(string_to_id(&s), Some(b));
        assert_eq!(string_to_id(&s.to_uppercase()), Some(b));
    }

    #[test]
    fn rejects_bad_ids() {
        assert_eq!(string_to_id("abc"), None);
        assert_eq!(string_to_id(&"1".repeat(52)), None); // '1' not in alphabet
        assert_eq!(string_to_id(&"a".repeat(60)), None);
    }

    #[test]
    fn code_roundtrip_with_formatting_noise() {
        let b = [7u8; 32];
        let id = id_to_string(&b);
        let code = make_code(&id);
        assert!(code.starts_with("LL-"));
        assert_eq!(parse_code(&code), Some(id.clone()));
        assert_eq!(parse_code(&code.to_lowercase().replace('-', " ")), Some(id.clone()));
        assert_eq!(parse_code(&format!("  {code}\n")), Some(id.clone()));
        assert_eq!(parse_code(&id), Some(id));
        assert_eq!(parse_code("LL-nope"), None);
        assert_eq!(parse_code(""), None);
    }

    #[test]
    fn frame_roundtrip() {
        let m = Msg::Roster { host: "h".into(), members: vec![RosterMember { id: "a".into(), connected: true }] };
        let f = encode_frame(&m);
        let len = u32::from_be_bytes([f[0], f[1], f[2], f[3]]) as usize;
        assert_eq!(len, f.len() - 4);
        assert_eq!(decode_body(&f[4..]).unwrap(), m);
        assert!(decode_body(b"garbage").is_err());
    }

    #[test]
    fn event_messages_are_chunked() {
        let m = Uuid::new_v4();
        let evs: Vec<Event> = (0..(EVENTS_PER_MSG * 2 + 5) as i64).map(|i| Event::new_chest("a", i, m)).collect();
        let msgs = event_msgs(evs);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn early_dialer_makes_the_next_host_take_over() {
        use std::collections::HashSet;
        let m: Vec<String> = ["host", "b", "c"].iter().map(|s| s.to_string()).collect();
        let none = HashSet::new();
        assert!(should_take_over(&m, &none, "host", "b"));
        assert!(!should_take_over(&m, &none, "host", "c"), "c is not next in line");
        // b already found unreachable: c is next
        let dead: HashSet<String> = ["b".to_string()].into_iter().collect();
        assert!(should_take_over(&m, &dead, "host", "c"));
    }

    #[test]
    fn handoff_picks_next_member_in_order() {
        let m: Vec<String> = ["host", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(next_host(&m, "host"), Some("b".into()));
        assert_eq!(next_host(&m[..1], "host"), None);
        let m2: Vec<String> = ["b", "host", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(next_host(&m2, "host"), Some("b".into()));
    }
}
