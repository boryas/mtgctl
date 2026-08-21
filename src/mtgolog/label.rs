//! Advisory deck labels for my side, guessed positively from marker cards
//! (definitions/mtgo/markers.toml). Stored in mtgo_match_labels with source = "guess"; a
//! list-backed labeller can later write source = "list" into the same table.

use super::db::{mtgo_game_cards, mtgo_match_labels, mtgo_matches, truncate};
use super::stats::ensure_view;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Integer, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct SubtypeRule {
    pub name: String,
    pub markers: Vec<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct ArchetypeRule {
    pub name: String,
    pub category: String,
    pub markers: Vec<String>,
    pub subtype: Vec<SubtypeRule>,
}

#[derive(Deserialize, Default)]
struct Markers {
    #[serde(default)]
    archetype: Vec<ArchetypeRule>,
}

pub fn load_markers(path: &Path) -> Result<Vec<ArchetypeRule>, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m: Markers = toml::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(m.archetype)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Guess {
    pub archetype: Option<String>,
    pub archetype_marker: Option<String>,
    pub subtype: Option<String>,
    pub subtype_marker: Option<String>,
    pub category: Option<String>,
}

/// Apply the ordered rules to a card set. First archetype whose markers hit wins; then the first
/// subtype rule under it. Nothing seen → None (explicitly unknown, never a default).
pub fn guess(rules: &[ArchetypeRule], cards: &BTreeSet<String>) -> Guess {
    let mut g = Guess::default();
    let Some((arch, marker)) = rules.iter().find_map(|a| a.markers.iter().find(|m| cards.contains(*m)).map(|m| (a, m.clone()))) else {
        return g;
    };
    g.archetype = Some(arch.name.clone());
    g.archetype_marker = Some(marker);
    g.category = Some(arch.category.clone());
    if let Some((st, m)) = arch.subtype.iter().find_map(|s| s.markers.iter().find(|m| cards.contains(*m)).map(|m| (s, m.clone()))) {
        g.subtype = Some(st.name.clone());
        g.subtype_marker = Some(m);
    }
    g
}

#[derive(Insertable)]
#[diesel(table_name = mtgo_match_labels)]
struct NewLabel {
    match_id: i32,
    side: &'static str,
    source: &'static str,
    archetype: Option<String>,
    subtype: Option<String>,
    category: Option<String>,
    archetype_marker: Option<String>,
    subtype_marker: Option<String>,
}

/// Signature per match for one side. My side: union of my cards over matches on the same day in
/// the same cluster (one registered list per session). Opponent: the match's cards only (one
/// observation of them). DFC back faces fold into the front face.
fn signatures(conn: &mut SqliteConnection, side: &str) -> QueryResult<HashMap<i32, BTreeSet<String>>> {
    let dfc = super::cluster::dfc_map(conn)?;
    let raw: Vec<(i32, String, Option<String>, Option<i32>, Option<i32>)> = mtgo_game_cards::table
        .inner_join(mtgo_matches::table.on(mtgo_matches::id.eq(mtgo_game_cards::match_id)))
        .filter(mtgo_matches::replay.eq(false))
        .filter(mtgo_game_cards::side.eq(side))
        .select((mtgo_game_cards::match_id, mtgo_game_cards::name, mtgo_matches::start_ts, mtgo_matches::my_cluster, mtgo_matches::opp_cluster))
        .load(conn)?;
    let merge = side == "me";
    let rows = raw.into_iter().map(|(mid, name, start, mc, oc)| (mid, name, start, if merge { mc } else { oc }));
    let mut per_match: HashMap<i32, BTreeSet<String>> = HashMap::new();
    let mut key_of: HashMap<i32, (String, i32)> = HashMap::new();
    for (mid, name, start, cluster) in rows {
        let name = dfc.get(&name).cloned().unwrap_or(name);
        per_match.entry(mid).or_default().insert(name);
        key_of.entry(mid).or_insert_with(|| (start.as_deref().map(|s| s[..10.min(s.len())].to_string()).unwrap_or_default(), cluster.unwrap_or(-1)));
    }
    if !merge {
        return Ok(per_match);
    }
    let mut merged: HashMap<(String, i32), BTreeSet<String>> = HashMap::new();
    for (mid, cards) in &per_match {
        merged.entry(key_of[mid].clone()).or_default().extend(cards.iter().cloned());
    }
    Ok(per_match.keys().map(|mid| (*mid, merged[&key_of[mid]].clone())).collect())
}

pub fn run_label(conn: &mut SqliteConnection, path: &Path, check: bool, suggest: Option<usize>) -> QueryResult<()> {
    super::db::ensure_schema(conn)?;
    let rules = match load_markers(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return Ok(());
        }
    };
    if let Some(min_size) = suggest {
        return suggest_markers(conn, &rules, min_size);
    }
    for side in ["me", "opp"] {
        let sigs = signatures(conn, side)?;
        let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
        let side_static: &'static str = if side == "me" { "me" } else { "opp" };
        conn.transaction::<_, diesel::result::Error, _>(|conn| {
            diesel::sql_query(format!("DELETE FROM mtgo_match_labels WHERE side = '{side}' AND source = 'guess'")).execute(conn)?;
            for (mid, cards) in &sigs {
                let g = guess(&rules, cards);
                *counts.entry((g.archetype.clone().unwrap_or("(unknown)".into()), g.subtype.clone().unwrap_or("(unknown)".into()))).or_default() += 1;
                diesel::insert_into(mtgo_match_labels::table)
                    .values(NewLabel { match_id: *mid, side: side_static, source: "guess", archetype: g.archetype, subtype: g.subtype, category: g.category, archetype_marker: g.archetype_marker, subtype_marker: g.subtype_marker })
                    .execute(conn)?;
            }
            Ok(())
        })?;
        let _ = truncate;
        let mut t = Table::new();
        t.load_preset(UTF8_FULL_CONDENSED);
        t.set_header([format!("{side}: guessed archetype"), "guessed subtype".into(), "matches".into()]);
        let mut rows: Vec<_> = counts.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        for ((a, s), n) in rows.iter().take(40) {
            t.add_row(vec![a.clone(), s.clone(), n.to_string()]);
        }
        println!("{t}");
        if rows.len() > 40 {
            println!("({} more label combinations)", rows.len() - 40);
        }
    }
    // Cluster fallback (opponent side): no marker seen, but the opponent's card cluster has a clear
    // majority archetype among its rule-labelled members → adopt it, marked "cluster" so it is
    // visibly second-hand. Subtype too when the majority agrees on it.
    #[derive(QueryableByName)]
    struct Member {
        #[diesel(sql_type = Integer)]
        match_id: i32,
        #[diesel(sql_type = Nullable<Integer>)]
        cluster: Option<i32>,
        #[diesel(sql_type = Nullable<Text>)]
        archetype: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        subtype: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        category: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        marker: Option<String>,
    }
    let members: Vec<Member> = diesel::sql_query(
        "SELECT m.id AS match_id, m.opp_cluster AS cluster, l.archetype, l.subtype, l.category, l.archetype_marker AS marker
         FROM mtgo_matches m LEFT JOIN mtgo_match_labels l ON l.match_id = m.id AND l.side = 'opp' AND l.source = 'guess'
         WHERE m.replay = 0",
    )
    .load(conn)?;
    let mut by_cluster: HashMap<i32, Vec<&Member>> = HashMap::new();
    for mm in &members {
        if let Some(c) = mm.cluster {
            by_cluster.entry(c).or_default().push(mm);
        }
    }
    let mut adopted = 0;
    for mm in members.iter().filter(|x| x.archetype.is_none() && x.cluster.is_some()) {
        let peers: Vec<&&Member> = by_cluster[&mm.cluster.unwrap()].iter().filter(|p| p.match_id != mm.match_id && p.archetype.is_some() && p.marker.as_deref() != Some("cluster")).collect();
        if peers.len() < 3 {
            continue;
        }
        let mut votes: HashMap<(&str, Option<&str>, Option<&str>), usize> = HashMap::new();
        let mut arch_votes: HashMap<&str, usize> = HashMap::new();
        for p in &peers {
            *arch_votes.entry(p.archetype.as_deref().unwrap()).or_default() += 1;
            *votes.entry((p.archetype.as_deref().unwrap(), p.subtype.as_deref(), p.category.as_deref())).or_default() += 1;
        }
        let (arch, n) = arch_votes.iter().max_by_key(|(_, n)| **n).unwrap();
        if (*n as f64) / (peers.len() as f64) < 0.6 {
            continue;
        }
        // subtype: majority among peers with this archetype
        let mut sub_votes: HashMap<Option<&str>, usize> = HashMap::new();
        let mut cat: Option<&str> = None;
        for ((a, s, c), k) in &votes {
            if a == arch {
                *sub_votes.entry(*s).or_default() += k;
                cat = cat.or(*c);
            }
        }
        let (sub, sn) = sub_votes.iter().max_by_key(|(_, k)| **k).unwrap();
        let sub = if (*sn as f64) / (*n as f64) >= 0.6 { *sub } else { None };
        let q = |v: Option<&str>| v.map(|x| format!("'{}'", x.replace('\'', "''"))).unwrap_or("NULL".into());
        diesel::sql_query(format!(
            "INSERT OR REPLACE INTO mtgo_match_labels(match_id, side, source, archetype, subtype, category, archetype_marker, subtype_marker) VALUES ({}, 'opp', 'guess', {}, {}, {}, 'cluster', {})",
            mm.match_id, q(Some(arch)), q(sub), q(cat), if sub.is_some() { "'cluster'" } else { "NULL" }
        ))
        .execute(conn)?;
        adopted += 1;
    }
    println!("cluster fallback: {adopted} opponent matches with no marker seen adopted their cluster's majority archetype (archetype_marker = 'cluster')");
    println!("advisory labels from marker cards (source = guess) → mtgo_match_labels; my_guess_* / opp_guess_* columns in mtgo_game_view");

    if check {
        check_against_hand_entries(conn, "me")?;
        check_against_hand_entries(conn, "opp")?;
    }
    Ok(())
}

#[derive(QueryableByName)]
struct ClusterRow {
    #[diesel(sql_type = BigInt)]
    id: i64,
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = BigInt)]
    size: i64,
    #[diesel(sql_type = Text)]
    top_cards: String,
    #[diesel(sql_type = BigInt)]
    mine: i64,
}

/// Print a markers.toml skeleton for clusters that no current rule labels.
fn suggest_markers(conn: &mut SqliteConnection, rules: &[ArchetypeRule], min_size: usize) -> QueryResult<()> {
    let rows: Vec<ClusterRow> = diesel::sql_query(
        "SELECT c.id, c.name, c.size, c.top_cards,
                (SELECT COUNT(*) FROM mtgo_matches m WHERE m.my_cluster = c.id) AS mine
         FROM mtgo_clusters c ORDER BY c.size DESC",
    )
    .load(conn)?;
    let mut n = 0;
    println!("# Suggested rule skeletons for clusters (size ≥ {min_size}) that no rule in markers.toml labels.");
    println!("# Fill in name/category (deck_types taxonomy: \"Archetype\" + optional subtype), prune markers, paste into markers.toml.\n");
    for c in rows.iter().filter(|c| c.size as usize >= min_size) {
        // top_cards: "Card 93%, Card 85%, ..." → names
        let cards: Vec<(String, String)> = c.top_cards.split(", ").filter_map(|x| x.rsplit_once(' ').map(|(name, pct)| (name.to_string(), pct.to_string()))).collect();
        let set: BTreeSet<String> = cards.iter().map(|(n, _)| n.clone()).collect();
        let g = guess(rules, &set);
        if g.archetype.is_some() {
            continue;
        }
        n += 1;
        println!("# cluster {} ({} members, {} mine): {}", c.id, c.size, c.mine, c.name);
        println!("# signature: {}", c.top_cards);
        println!("[[archetype]]\nname = \"???\"\ncategory = \"???\"");
        let markers: Vec<String> = cards.iter().take(5).map(|(n, _)| format!("\"{}\"", n.replace('"', "\\\""))).collect();
        println!("markers = [{}]\n", markers.join(", "));
    }
    if n == 0 {
        println!("# every cluster of size ≥ {min_size} is already labelled by a rule");
    }
    Ok(())
}

#[derive(QueryableByName)]
struct CheckRow {
    #[diesel(sql_type = Text)]
    hand_deck: String,
    #[diesel(sql_type = Nullable<Text>)]
    g_arch: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    g_sub: Option<String>,
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// Compare guesses with the hand-entered `matches` table, linked by (date, opponent):
/// my side against `deck_name`, the opponent against `opponent_deck`.
fn check_against_hand_entries(conn: &mut SqliteConnection, side: &str) -> QueryResult<()> {
    ensure_view(conn)?;
    let hand_col = if side == "me" { "h.deck_name" } else { "h.opponent_deck" };
    let rows: Vec<CheckRow> = diesel::sql_query(format!(
        "SELECT {hand_col} AS hand_deck, l.archetype AS g_arch, l.subtype AS g_sub, COUNT(*) AS n
         FROM matches h
         JOIN mtgo_matches m ON substr(m.start_ts,1,10) = h.date AND lower(m.opponent) = lower(h.opponent_name) AND m.replay = 0
         LEFT JOIN mtgo_match_labels l ON l.match_id = m.id AND l.side = '{side}' AND l.source = 'guess'
         GROUP BY {hand_col}, l.archetype, l.subtype ORDER BY n DESC"
    ))
    .load(conn)?;
    println!("\n== {} vs hand-entered {}", if side == "me" { "my guess" } else { "opponent guess" }, if side == "me" { "deck_name" } else { "opponent_deck" });
    let (mut total, mut arch_ok, mut sub_known, mut sub_ok, mut arch_known) = (0, 0, 0, 0, 0);
    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED);
    t.set_header(["hand-entered deck", "guessed archetype", "guessed subtype", "n", "archetype", "subtype"]);
    let mut shown = 0;
    for r in &rows {
        let (h_arch, h_sub) = crate::game::parse_deck_name(&r.hand_deck);
        let a_ok = r.g_arch.as_deref() == Some(h_arch);
        let s_ok = r.g_sub.is_some() && r.g_sub.as_deref() == h_sub;
        total += r.n;
        if r.g_arch.is_some() {
            arch_known += r.n;
            if a_ok {
                arch_ok += r.n;
            }
        }
        if r.g_sub.is_some() {
            sub_known += r.n;
            if s_ok {
                sub_ok += r.n;
            }
        }
        shown += 1;
        if shown > 45 {
            continue;
        }
        t.add_row(vec![
            r.hand_deck.clone(),
            r.g_arch.clone().unwrap_or("(unknown)".into()),
            r.g_sub.clone().unwrap_or("(unknown)".into()),
            r.n.to_string(),
            if r.g_arch.is_none() { "?" } else if a_ok { "✓" } else { "✗" }.into(),
            if r.g_sub.is_none() { "?" } else if s_ok { "✓" } else { "✗" }.into(),
        ]);
    }
    println!("{t}");
    let pct = |a: i64, b: i64| if b > 0 { format!("{:.1}%", 100.0 * a as f64 / b as f64) } else { "n/a".into() };
    println!(
        "linked hand-entered matches: {total}. archetype guessed for {arch_known} ({}), correct {}; subtype guessed for {sub_known} ({}), correct {}",
        pct(arch_known, total),
        pct(arch_ok, arch_known),
        pct(sub_known, total),
        pct(sub_ok, sub_known)
    );
    Ok(())
}
