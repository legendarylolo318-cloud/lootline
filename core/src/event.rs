//! Append-only event log, merged by set union.
//!
//! Every event carries a globally unique `id`. Two peers converge by exchanging the events
//! the other is missing; merge is commutative, associative and idempotent.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

/// Stable identifier for a player (the iroh endpoint id in a party, a local uuid otherwise).
pub type PlayerId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Epic,
    Legendary,
    Mythic,
}

impl Rarity {
    pub const ALL: [Rarity; 6] =
        [Rarity::Common, Rarity::Uncommon, Rarity::Rare, Rarity::Epic, Rarity::Legendary, Rarity::Mythic];
    pub fn name(self) -> &'static str {
        match self {
            Rarity::Common => "Common",
            Rarity::Uncommon => "Uncommon",
            Rarity::Rare => "Rare",
            Rarity::Epic => "Epic",
            Rarity::Legendary => "Legendary",
            Rarity::Mythic => "Mythic",
        }
    }
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn from_index(i: usize) -> Option<Rarity> {
        Self::ALL.get(i).copied()
    }
    pub fn from_name(s: &str) -> Option<Rarity> {
        Self::ALL.iter().copied().find(|r| r.name().eq_ignore_ascii_case(s))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    ChestOpened {
        id: Uuid,
        player: PlayerId,
        ts: i64,
        rarity: Option<Rarity>,
        item: Option<String>,
        match_id: Uuid,
    },
    /// Retroactively tag a chest (the rarity overlay appears *after* the chest is counted).
    Tag {
        id: Uuid,
        target: Uuid,
        player: PlayerId,
        ts: i64,
        rarity: Option<Rarity>,
        item: Option<String>,
    },
    Undo {
        id: Uuid,
        target: Uuid,
        player: PlayerId,
        ts: i64,
    },
    Join {
        id: Uuid,
        player: PlayerId,
        name: String,
        color: String,
        detecting: bool,
        ts: i64,
    },
    Leave {
        id: Uuid,
        player: PlayerId,
        ts: i64,
    },
    Rename {
        id: Uuid,
        player: PlayerId,
        name: String,
        ts: i64,
    },
}

impl Event {
    pub fn id(&self) -> Uuid {
        match self {
            Event::ChestOpened { id, .. }
            | Event::Tag { id, .. }
            | Event::Undo { id, .. }
            | Event::Join { id, .. }
            | Event::Leave { id, .. }
            | Event::Rename { id, .. } => *id,
        }
    }
    pub fn ts(&self) -> i64 {
        match self {
            Event::ChestOpened { ts, .. }
            | Event::Tag { ts, .. }
            | Event::Undo { ts, .. }
            | Event::Join { ts, .. }
            | Event::Leave { ts, .. }
            | Event::Rename { ts, .. } => *ts,
        }
    }
    pub fn player(&self) -> &str {
        match self {
            Event::ChestOpened { player, .. }
            | Event::Tag { player, .. }
            | Event::Undo { player, .. }
            | Event::Join { player, .. }
            | Event::Leave { player, .. }
            | Event::Rename { player, .. } => player,
        }
    }
    pub fn new_chest(player: &str, ts: i64, match_id: Uuid) -> Event {
        Event::ChestOpened { id: Uuid::new_v4(), player: player.to_string(), ts, rarity: None, item: None, match_id }
    }
}

/// A chest after applying undos and tags.
#[derive(Clone, Debug, PartialEq)]
pub struct Chest {
    pub id: Uuid,
    pub player: PlayerId,
    pub ts: i64,
    pub rarity: Option<Rarity>,
    pub item: Option<String>,
    pub match_id: Uuid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    pub color: String,
    pub detecting: bool,
    /// Currently a member (last Join/Leave decides).
    pub active: bool,
}

#[derive(Default, Clone, Debug)]
pub struct EventLog {
    events: HashMap<Uuid, Event>,
}

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    pub fn contains(&self, id: &Uuid) -> bool {
        self.events.contains_key(id)
    }
    pub fn get(&self, id: &Uuid) -> Option<&Event> {
        self.events.get(id)
    }
    pub fn ids(&self) -> Vec<Uuid> {
        self.events.keys().copied().collect()
    }
    pub fn all(&self) -> Vec<Event> {
        let mut v: Vec<Event> = self.events.values().cloned().collect();
        v.sort_by_key(|e| (e.ts(), e.id()));
        v
    }

    /// Insert one event. Returns true if it was new.
    pub fn insert(&mut self, e: Event) -> bool {
        if self.events.contains_key(&e.id()) {
            return false;
        }
        self.events.insert(e.id(), e);
        true
    }

    /// Merge by set union; returns the events that were new.
    pub fn merge<I: IntoIterator<Item = Event>>(&mut self, it: I) -> Vec<Event> {
        let mut added = Vec::new();
        for e in it {
            if self.insert(e.clone()) {
                added.push(e);
            }
        }
        added
    }

    /// Events the peer (described by the ids it holds) is missing.
    pub fn missing_for(&self, have: &HashSet<Uuid>) -> Vec<Event> {
        self.all().into_iter().filter(|e| !have.contains(&e.id())).collect()
    }

    /// Effective chests: opened, not undone, with the latest tag applied. Sorted by time.
    ///
    /// Undo and Tag events only apply when they come from the chest's owner, so a buggy or
    /// hostile peer cannot erase or relabel somebody else's chests. Undo/Tag events whose target
    /// is unknown (not received yet, or never existed) are kept but have no effect; they apply
    /// as soon as the chest arrives, whatever the arrival order.
    pub fn chests(&self) -> Vec<Chest> {
        let undone: HashSet<(Uuid, &str)> = self
            .events
            .values()
            .filter_map(|e| match e {
                Event::Undo { target, player, .. } => Some((*target, player.as_str())),
                _ => None,
            })
            .collect();
        // latest tag per (chest, tagger) ((ts, id) ordering gives the same winner on every peer)
        type TagEntry = (i64, Uuid, Option<Rarity>, Option<String>);
        let mut tags: HashMap<(Uuid, &str), TagEntry> = HashMap::new();
        for e in self.events.values() {
            if let Event::Tag { id, target, ts, rarity, item, player } = e {
                let key = (*target, player.as_str());
                let better = tags.get(&key).is_none_or(|(t, i, _, _)| (*ts, *id) > (*t, *i));
                if better {
                    tags.insert(key, (*ts, *id, *rarity, item.clone()));
                }
            }
        }
        let mut out: Vec<Chest> = self
            .events
            .values()
            .filter_map(|e| match e {
                Event::ChestOpened { id, player, ts, rarity, item, match_id }
                    if !undone.contains(&(*id, player.as_str())) =>
                {
                    let (r, it) = match tags.get(&(*id, player.as_str())) {
                        Some((_, _, r, it)) => (*r, it.clone()),
                        None => (*rarity, item.clone()),
                    };
                    Some(Chest { id: *id, player: player.clone(), ts: *ts, rarity: r, item: it, match_id: *match_id })
                }
                _ => None,
            })
            .collect();
        out.sort_by_key(|c| (c.ts, c.id));
        out
    }

    /// Total chests per player, and the party total (the sum over everyone).
    pub fn chest_counts(&self) -> (BTreeMap<PlayerId, u32>, u32) {
        let mut m: BTreeMap<PlayerId, u32> = BTreeMap::new();
        for c in self.chests() {
            *m.entry(c.player).or_default() += 1;
        }
        let total = m.values().sum();
        (m, total)
    }

    /// The most recent not-yet-undone chest by `player`, for the "undo last" hotkey.
    pub fn last_chest_of(&self, player: &str) -> Option<Uuid> {
        self.chests().into_iter().rev().find(|c| c.player == player).map(|c| c.id)
    }

    /// Timestamp for a new Join / Leave / Rename about `player`: `now`, but strictly after every
    /// membership event already logged for that player. Roster order is decided by (ts, id), so
    /// without this a peer whose clock runs behind (e.g. a host kicking a player whose clock is
    /// ahead) would create a Leave that sorts *before* the Join it is meant to override.
    pub fn next_membership_ts(&self, player: &str, now: i64) -> i64 {
        let last = self
            .events
            .values()
            .filter(|e| {
                matches!(e, Event::Join { .. } | Event::Leave { .. } | Event::Rename { .. }) && e.player() == player
            })
            .map(|e| e.ts())
            .max();
        match last {
            Some(t) if t >= now => t.saturating_add(1),
            _ => now,
        }
    }

    /// Roster derived from Join / Leave / Rename events (ordered by (ts, id)).
    pub fn players(&self) -> Vec<PlayerInfo> {
        let mut evs: Vec<&Event> = self.events.values().collect();
        evs.sort_by_key(|e| (e.ts(), e.id()));
        let mut map: BTreeMap<PlayerId, PlayerInfo> = BTreeMap::new();
        for e in evs {
            match e {
                Event::Join { player, name, color, detecting, .. } => {
                    map.insert(
                        player.clone(),
                        PlayerInfo {
                            id: player.clone(),
                            name: name.clone(),
                            color: color.clone(),
                            detecting: *detecting,
                            active: true,
                        },
                    );
                }
                Event::Leave { player, .. } => {
                    if let Some(p) = map.get_mut(player) {
                        p.active = false;
                    }
                }
                Event::Rename { player, name, .. } => {
                    if let Some(p) = map.get_mut(player) {
                        p.name = name.clone();
                    }
                }
                _ => {}
            }
        }
        map.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chest(p: &str, ts: i64, m: Uuid) -> Event {
        Event::new_chest(p, ts, m)
    }

    #[test]
    fn merge_is_idempotent_and_commutative() {
        let m = Uuid::new_v4();
        let a: Vec<Event> = (0..5).map(|i| chest("a", i, m)).collect();
        let b: Vec<Event> = (0..4).map(|i| chest("b", 10 + i, m)).collect();
        let mut l1 = EventLog::new();
        l1.merge(a.clone());
        l1.merge(b.clone());
        l1.merge(a.clone());
        let mut l2 = EventLog::new();
        l2.merge(b);
        l2.merge(a);
        assert_eq!(l1.len(), 9);
        assert_eq!(l1.all(), l2.all());
        assert_eq!(l1.chest_counts(), l2.chest_counts());
    }

    #[test]
    fn merge_reports_only_new() {
        let m = Uuid::new_v4();
        let e = chest("a", 1, m);
        let mut l = EventLog::new();
        assert_eq!(l.merge(vec![e.clone()]).len(), 1);
        assert_eq!(l.merge(vec![e]).len(), 0);
    }

    #[test]
    fn undo_removes_chest_regardless_of_arrival_order() {
        let m = Uuid::new_v4();
        let c = chest("a", 1, m);
        let u = Event::Undo { id: Uuid::new_v4(), target: c.id(), player: "a".into(), ts: 2 };
        let mut l1 = EventLog::new();
        l1.merge(vec![c.clone(), u.clone()]);
        let mut l2 = EventLog::new();
        l2.merge(vec![u, c]);
        assert!(l1.chests().is_empty());
        assert!(l2.chests().is_empty());
    }

    #[test]
    fn party_total_is_sum_of_everyone() {
        let m = Uuid::new_v4();
        let mut l = EventLog::new();
        for i in 0..3 {
            l.insert(chest("a", i, m));
        }
        for i in 0..4 {
            l.insert(chest("b", 10 + i, m));
        }
        l.insert(chest("c", 20, m));
        let (per, total) = l.chest_counts();
        assert_eq!(per["a"], 3);
        assert_eq!(per["b"], 4);
        assert_eq!(per["c"], 1);
        assert_eq!(total, 8);
    }

    #[test]
    fn latest_tag_wins_deterministically() {
        let m = Uuid::new_v4();
        let c = chest("a", 1, m);
        let t = |ts, r| Event::Tag {
            id: Uuid::new_v4(),
            target: c.id(),
            player: "a".into(),
            ts,
            rarity: Some(r),
            item: None,
        };
        let t1 = t(5, Rarity::Rare);
        let t2 = t(9, Rarity::Mythic);
        let mut l1 = EventLog::new();
        l1.merge(vec![c.clone(), t1.clone(), t2.clone()]);
        let mut l2 = EventLog::new();
        l2.merge(vec![t2, c, t1]);
        assert_eq!(l1.chests()[0].rarity, Some(Rarity::Mythic));
        assert_eq!(l2.chests()[0].rarity, Some(Rarity::Mythic));
    }

    #[test]
    fn missing_for_returns_difference() {
        let m = Uuid::new_v4();
        let mut l = EventLog::new();
        let evs: Vec<Event> = (0..5).map(|i| chest("a", i, m)).collect();
        l.merge(evs.clone());
        let have: HashSet<Uuid> = evs.iter().take(3).map(|e| e.id()).collect();
        assert_eq!(l.missing_for(&have).len(), 2);
    }

    #[test]
    fn roster_tracks_join_leave_rename() {
        let mut l = EventLog::new();
        l.insert(Event::Join {
            id: Uuid::new_v4(),
            player: "p".into(),
            name: "Old".into(),
            color: "#fff".into(),
            detecting: true,
            ts: 1,
        });
        l.insert(Event::Rename { id: Uuid::new_v4(), player: "p".into(), name: "New".into(), ts: 2 });
        assert_eq!(l.players()[0].name, "New");
        assert!(l.players()[0].active);
        l.insert(Event::Leave { id: Uuid::new_v4(), player: "p".into(), ts: 3 });
        assert!(!l.players()[0].active);
    }

    #[test]
    fn last_chest_of_skips_undone() {
        let m = Uuid::new_v4();
        let mut l = EventLog::new();
        let a = chest("a", 1, m);
        let b = chest("a", 2, m);
        l.merge(vec![a.clone(), b.clone()]);
        assert_eq!(l.last_chest_of("a"), Some(b.id()));
        l.insert(Event::Undo { id: Uuid::new_v4(), target: b.id(), player: "a".into(), ts: 3 });
        assert_eq!(l.last_chest_of("a"), Some(a.id()));
    }

    #[test]
    fn undo_and_tag_from_another_player_are_ignored() {
        let m = Uuid::new_v4();
        let c = chest("a", 1, m);
        let mut l = EventLog::new();
        l.insert(c.clone());
        l.insert(Event::Undo { id: Uuid::new_v4(), target: c.id(), player: "mallory".into(), ts: 2 });
        l.insert(Event::Tag {
            id: Uuid::new_v4(),
            target: c.id(),
            player: "mallory".into(),
            ts: 3,
            rarity: Some(Rarity::Mythic),
            item: None,
        });
        assert_eq!(l.chests().len(), 1);
        assert_eq!(l.chests()[0].rarity, None);
    }

    #[test]
    fn undo_of_unknown_id_is_harmless_and_applies_late() {
        let m = Uuid::new_v4();
        let c = chest("a", 1, m);
        let mut l = EventLog::new();
        l.insert(chest("a", 0, m));
        l.insert(Event::Undo { id: Uuid::new_v4(), target: Uuid::new_v4(), player: "a".into(), ts: 2 });
        let late = Event::Undo { id: Uuid::new_v4(), target: c.id(), player: "a".into(), ts: 3 };
        l.insert(late);
        assert_eq!(l.chests().len(), 1);
        // the target arrives after its undo: still undone
        l.insert(c);
        assert_eq!(l.chests().len(), 1);
    }

    #[test]
    fn duplicate_delivery_does_not_double_count() {
        let m = Uuid::new_v4();
        let c = chest("a", 1, m);
        let mut l = EventLog::new();
        for _ in 0..5 {
            l.merge(vec![c.clone(), c.clone()]);
        }
        assert_eq!(l.chest_counts().1, 1);
    }

    #[test]
    fn kick_from_a_slow_clock_still_wins() {
        let mut l = EventLog::new();
        // player's clock is 10 minutes ahead of the host's
        l.insert(Event::Join {
            id: Uuid::new_v4(),
            player: "p".into(),
            name: "P".into(),
            color: "#fff".into(),
            detecting: false,
            ts: 1_000_600_000,
        });
        let host_now = 1_000_000_000;
        let ts = l.next_membership_ts("p", host_now);
        assert!(ts > 1_000_600_000);
        l.insert(Event::Leave { id: Uuid::new_v4(), player: "p".into(), ts });
        assert!(!l.players()[0].active);
        // with no prior events the wall clock is used unchanged
        assert_eq!(l.next_membership_ts("nobody", host_now), host_now);
    }
}
