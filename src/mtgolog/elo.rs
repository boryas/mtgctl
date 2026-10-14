//! Elo over (player, deck) entities from the LDC: a pilot on a deck is one rated thing, so
//! `boryas-Doomsday` and `boryas-Doomsday:Vroomsday` are separate. Challenge matches only.
//! Matches are replayed by (date, round); all matches of one round update together, so the
//! order sheets list pairings in cannot matter. Draws score 0.5.
//! A regularized Bradley-Terry fit (order-independent) is reported on the same scale as a check.

use diesel::prelude::*;
use diesel::sql_types::{Integer, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use serde::Serialize;
use std::collections::HashMap;

/// K while an entity has fewer than `PROVISIONAL` matches, then after.
const K_PROVISIONAL: f64 = 40.0;
const K_STABLE: f64 = 24.0;
const PROVISIONAL: usize = 20;
/// Virtual matches (half won, half lost) against a 1500 opponent in the Bradley-Terry fit, so an
/// undefeated entity has a finite strength.
const BT_PRIOR: f64 = 2.0;
const BT_ITERS: usize = 200;
/// LDC notes that name a variant worth rating apart from the base deck.
const VARIANT_NOTES: &[(&str, &str)] = &[("vroomsday", "Vroomsday"), ("car", "Car"), ("car doomsday", "Car")];

#[derive(Serialize)]
pub struct Entity {
    pub player: String,
    pub deck: String,
    pub n: usize,
    pub wins: f64,
    pub elo: f64,
    pub peak: f64,
    pub bt: f64,
    pub last: String,
}

#[derive(Serialize)]
pub struct EloResult {
    pub matches: usize,
    pub first: String,
    pub last: String,
    pub entities: Vec<Entity>,
}

#[derive(QueryableByName)]
struct Row {
    #[diesel(sql_type = Text)]
    date: String,
    #[diesel(sql_type = Integer)]
    round: i32,
    #[diesel(sql_type = Text)]
    a: String,
    #[diesel(sql_type = Text)]
    b: String,
    #[diesel(sql_type = Integer)]
    gw: i32,
    #[diesel(sql_type = Integer)]
    gl: i32,
    #[diesel(sql_type = Nullable<Text>)]
    a_deck: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    a_notes: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    b_deck: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    b_notes: Option<String>,
}

fn deck_label(deck: Option<&str>, notes: Option<&str>) -> String {
    let deck = deck.unwrap_or("?");
    let note = notes.unwrap_or("").trim().to_lowercase();
    match VARIANT_NOTES.iter().find(|(n, _)| *n == note) {
        Some((_, v)) => format!("{deck}:{v}"),
        None => deck.to_string(),
    }
}

pub fn compute(conn: &mut SqliteConnection, event_type: &str) -> Result<EloResult, String> {
    let rows: Vec<Row> = diesel::sql_query(
        "SELECT e.date, m.round, lower(m.player) AS a, lower(m.opponent) AS b, m.game_wins AS gw, m.game_losses AS gl,
                p.deck AS a_deck, p.notes AS a_notes, o.deck AS b_deck, o.notes AS b_notes
         FROM ldc_matches m
         JOIN ldc_events e ON e.event = m.event
         LEFT JOIN ldc_players p ON p.event = m.event AND p.player_key = lower(m.player)
         LEFT JOIN ldc_players o ON o.event = m.event AND o.player_key = lower(m.opponent)
         WHERE e.event_type = ?
         ORDER BY e.date, m.round",
    )
    .bind::<Text, _>(event_type)
    .load(conn)
    .map_err(|e| e.to_string())?;

    // each match is stored once per side; keep the first of the pair
    let mut seen: std::collections::HashSet<(String, i32, String, String)> = Default::default();
    let mut matches: Vec<(String, i32, usize, usize, f64)> = Vec::new();
    let mut ids: HashMap<(String, String), usize> = HashMap::new();
    let mut ents: Vec<Entity> = Vec::new();
    let mut id_of = |player: &str, deck: String, ents: &mut Vec<Entity>| -> usize {
        *ids.entry((player.to_string(), deck.clone())).or_insert_with(|| {
            ents.push(Entity { player: player.to_string(), deck, n: 0, wins: 0.0, elo: 1500.0, peak: 1500.0, bt: 1500.0, last: String::new() });
            ents.len() - 1
        })
    };
    for r in &rows {
        let (lo, hi) = if r.a <= r.b { (&r.a, &r.b) } else { (&r.b, &r.a) };
        if !seen.insert((r.date.clone(), r.round, lo.clone(), hi.clone())) {
            continue;
        }
        let s = if r.gw > r.gl { 1.0 } else if r.gw < r.gl { 0.0 } else { 0.5 };
        let a = id_of(&r.a, deck_label(r.a_deck.as_deref(), r.a_notes.as_deref()), &mut ents);
        let b = id_of(&r.b, deck_label(r.b_deck.as_deref(), r.b_notes.as_deref()), &mut ents);
        matches.push((r.date.clone(), r.round, a, b, s));
    }

    let mut i = 0;
    while i < matches.len() {
        let mut j = i;
        while j < matches.len() && matches[j].0 == matches[i].0 && matches[j].1 == matches[i].1 {
            j += 1;
        }
        let mut delta: HashMap<usize, f64> = HashMap::new();
        for (date, _, a, b, s) in &matches[i..j] {
            let (a, b) = (*a, *b);
            let ea = 1.0 / (1.0 + 10f64.powf((ents[b].elo - ents[a].elo) / 400.0));
            let k = |n: usize| if n < PROVISIONAL { K_PROVISIONAL } else { K_STABLE };
            *delta.entry(a).or_default() += k(ents[a].n) * (s - ea);
            *delta.entry(b).or_default() += k(ents[b].n) * ((1.0 - s) - (1.0 - ea));
            for (e, w) in [(a, *s), (b, 1.0 - s)] {
                ents[e].n += 1;
                ents[e].wins += w;
                ents[e].last = date.clone();
            }
        }
        for (e, d) in delta {
            ents[e].elo += d;
            ents[e].peak = ents[e].peak.max(ents[e].elo);
        }
        i = j;
    }

    bradley_terry(&mut ents, &matches);
    ents.sort_by(|x, y| y.elo.total_cmp(&x.elo));
    Ok(EloResult {
        matches: matches.len(),
        first: matches.first().map(|m| m.0.clone()).unwrap_or_default(),
        last: matches.last().map(|m| m.0.clone()).unwrap_or_default(),
        entities: ents,
    })
}

/// Minorization-maximization for Bradley-Terry strengths, normalized to geometric mean 1 and
/// written as 1500 + 400·log10(strength) so a 400-point gap is 10:1 odds, as in Elo.
fn bradley_terry(ents: &mut [Entity], matches: &[(String, i32, usize, usize, f64)]) {
    let n = ents.len();
    let mut pair: HashMap<(usize, usize), (f64, f64)> = HashMap::new(); // (a's wins, games) with a < b
    for &(_, _, a, b, s) in matches {
        let (lo, hi, w) = if a < b { (a, b, s) } else { (b, a, 1.0 - s) };
        let e = pair.entry((lo, hi)).or_default();
        e.0 += w;
        e.1 += 1.0;
    }
    let mut opp: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n]; // (opponent, games)
    for (&(a, b), &(_, g)) in &pair {
        opp[a].push((b, g));
        opp[b].push((a, g));
    }
    let mut p = vec![1.0f64; n];
    for _ in 0..BT_ITERS {
        let mut next = vec![0.0f64; n];
        for a in 0..n {
            let num = ents[a].wins + BT_PRIOR / 2.0;
            let mut den = BT_PRIOR / (p[a] + 1.0);
            for &(b, g) in &opp[a] {
                den += g / (p[a] + p[b]);
            }
            next[a] = num / den;
        }
        let g = (next.iter().map(|v| v.ln()).sum::<f64>() / n as f64).exp();
        for v in &mut next {
            *v /= g;
        }
        p = next;
    }
    for (e, s) in ents.iter_mut().zip(p) {
        e.bt = 1500.0 + 400.0 * s.log10();
    }
}
