//! Scope selection and per-item / per-rarity odds rows.

use crate::event::{Chest, Rarity};
use crate::loot::LootTable;
use crate::odds;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Match,
    Session,
    Lifetime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Me,
    Party,
}

/// Scopes are time windows, so friends whose matches have different ids still line up:
/// "this match" = everything since *my* current match started.
pub struct ScopeCtx<'a> {
    pub me: &'a str,
    pub match_start: i64,
    pub session_start: i64,
}

pub fn select<'a>(chests: &'a [Chest], scope: Scope, who: Who, ctx: &ScopeCtx) -> Vec<&'a Chest> {
    chests
        .iter()
        .filter(|c| match scope {
            Scope::Match => c.ts >= ctx.match_start,
            Scope::Session => c.ts >= ctx.session_start,
            Scope::Lifetime => true,
        })
        .filter(|c| who == Who::Party || c.player == ctx.me)
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct OddsRow {
    pub name: String,
    pub rarity: Rarity,
    pub is_rarity_row: bool,
    pub stat: odds::Stat,
}

/// One row per item of the loot table.
/// `n` is the number of chests in scope; `got` counts tagged chests.
pub fn odds_rows(table: &LootTable, chests: &[&Chest]) -> Vec<OddsRow> {
    let n = chests.len() as u64;
    let mut by_item: BTreeMap<&str, u64> = BTreeMap::new();
    for c in chests {
        if let Some(i) = &c.item {
            *by_item.entry(i.as_str()).or_default() += 1;
        }
    }
    let mut rows = Vec::new();
    for it in &table.items {
        let stat = odds::stat(n, it.p, by_item.get(it.name.as_str()).copied().unwrap_or(0));
        rows.push(OddsRow { name: it.name.clone(), rarity: it.rarity, is_rarity_row: false, stat });
    }
    rows
}

/// Count of chests per rarity (index = `Rarity::index`) plus untagged.
pub fn rarity_distribution(chests: &[&Chest]) -> ([u32; 6], u32) {
    let mut d = [0u32; 6];
    let mut untagged = 0;
    for c in chests {
        match c.rarity {
            Some(r) => d[r.index()] += 1,
            None => untagged += 1,
        }
    }
    (d, untagged)
}

/// Overall luck 0..=1 (0.5 = exactly average): the mean mid-p luck percentile across every
/// item in the table, given how many chests are in scope.
pub fn overall_luck(table: &LootTable, chests: &[&Chest]) -> f64 {
    let n = chests.len() as u64;
    if n == 0 || table.items.is_empty() {
        return 0.5;
    }
    let sum: f64 = table
        .items
        .iter()
        .map(|it| {
            let got = chests.iter().filter(|c| c.item.as_deref() == Some(it.name.as_str())).count() as u64;
            odds::luck_midp(n, it.p, got)
        })
        .sum();
    sum / table.items.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ch(player: &str, ts: i64, m: Uuid, r: Option<Rarity>, item: Option<&str>) -> Chest {
        Chest { id: Uuid::new_v4(), player: player.into(), ts, rarity: r, item: item.map(String::from), match_id: m }
    }

    #[test]
    fn scope_and_who_filtering() {
        let m1 = Uuid::new_v4();
        let m2 = Uuid::new_v4();
        let cs = vec![
            ch("me", 10, m1, None, None),
            ch("fr", 11, m1, None, None),
            ch("me", 20, m2, None, None),
            ch("fr", 5, m2, None, None),
        ];
        let ctx = ScopeCtx { me: "me", match_start: 20, session_start: 10 };
        assert_eq!(select(&cs, Scope::Match, Who::Party, &ctx).len(), 1);
        assert_eq!(select(&cs, Scope::Match, Who::Me, &ctx).len(), 1);
        assert_eq!(select(&cs, Scope::Session, Who::Party, &ctx).len(), 3);
        assert_eq!(select(&cs, Scope::Lifetime, Who::Party, &ctx).len(), 4);
        assert_eq!(select(&cs, Scope::Lifetime, Who::Me, &ctx).len(), 2);
    }

    #[test]
    fn odds_rows_count_hits() {
        let t = LootTable::parse(
            r#"{"season":"s","items":[{"name":"A","rarity":"Rare","p":0.5},{"name":"B","rarity":"Common","p":0.5}]}"#,
        )
        .unwrap();
        let m = Uuid::new_v4();
        let cs = [ch("a", 1, m, Some(Rarity::Rare), Some("A")), ch("a", 2, m, Some(Rarity::Common), None)];
        let refs: Vec<&Chest> = cs.iter().collect();
        let rows = odds_rows(&t, &refs);
        let a = rows.iter().find(|r| r.name == "A").unwrap();
        assert_eq!((a.stat.n, a.stat.got), (2, 1));
        assert!(rows.iter().all(|r| !r.name.starts_with("Any ")));
        let (d, un) = rarity_distribution(&refs);
        assert_eq!((d[Rarity::Rare.index()], d[Rarity::Common.index()], un), (1, 1, 0));
    }

    #[test]
    fn overall_luck_with_no_data_is_neutral() {
        let t = LootTable::bundled();
        assert_eq!(overall_luck(&t, &[]), 0.5);
    }
}
