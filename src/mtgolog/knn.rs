//! Nearest-anchor deck labels for opponents (my own side is a whole-deck signature, which the
//! marker rules in label.rs read at 99%; see `run`). Every (match, side) is a point in the TF-IDF
//! space of the cards it showed; a match is labelled by the similarity-weighted vote of its k
//! nearest *anchors*, matches whose deck is known independently of the cards: the LDC sheet for
//! the event, a hand-entered match, my registered list, a manual label. No card is decisive on its
//! own — the whole signature votes, so one sideboard Torpor Orb cannot outvote fifteen Lands
//! cards. Below the vote margin the match stays unknown: an opponent who never showed a
//! distinguishing card is genuinely undetermined, and coverage, not accuracy, is the number that
//! moves. When nothing observed is near at all, archetype *skeletons* (published card breakdowns,
//! skeleton.rs) vote instead, visibly marked as the weaker evidence.

use super::cluster::{dot, load_docs, vectorize_with, Doc, Vectors};
use super::db::mtgo_match_labels;
use super::label::NewLabel;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use diesel::prelude::*;
use diesel::sql_types::{Nullable, Text};
use diesel::sqlite::SqliteConnection;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Similarity exponent: near anchors dominate far ones (chosen by leave-one-out accuracy).
const POWER: f32 = 3.0;
/// A vote below this margin is labelled but listed by `--explain --uncertain`.
const UNCERTAIN_MARGIN: f32 = 0.6;
/// A signature this large that resembles nothing known is an off-meta deck ("Other"), not an
/// unobserved one: the median opponent shows 16 distinct cards, the bottom decile under 10.
const MIN_CARDS_FOR_OTHER: usize = 10;
/// An "Other" anchor votes only as a near-duplicate: off-meta decks resemble each other no more
/// than they resemble the field, so a loose match to one is not evidence of anything.
const OTHER_ANCHOR_MIN_SIM: f32 = 0.6;

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct LdcDeck {
    pub name: String,
    pub goldfish: Option<String>,
    pub archetype: String,
    pub subtype: Option<String>,
    pub category: Option<String>,
    pub ignore: bool,
}

/// A hand-entered deck name folded into the vocabulary ("Mystic Forge" → Tron / Mystic Forge).
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Alias {
    pub hand: String,
    pub archetype: String,
    pub subtype: Option<String>,
}

#[derive(Deserialize, Default)]
struct LdcDecks {
    #[serde(default)]
    deck: Vec<LdcDeck>,
    #[serde(default)]
    alias: Vec<Alias>,
}

pub struct Vocab {
    pub ldc: HashMap<String, LdcDeck>,
    pub goldfish: HashMap<String, LdcDeck>,
    pub alias: HashMap<String, Alias>,
}

pub fn load_ldc_map(path: &Path) -> Result<Vocab, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let d: LdcDecks = toml::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
    let goldfish = d.deck.iter().filter(|d| d.goldfish.is_some()).map(|d| (d.goldfish.clone().unwrap(), d.clone())).collect();
    Ok(Vocab { ldc: d.deck.into_iter().map(|d| (d.name.clone(), d)).collect(), goldfish, alias: d.alias.into_iter().map(|a| (a.hand.clone(), a)).collect() })
}

/// Colour words → WUBRG letters. One nomenclature everywhere: subtypes that name colours are
/// letters ("UB", "URW"); subtypes that name a build ("Turbo", "Yorion") stay words.
pub fn canon_subtype(s: &str) -> String {
    const NAMES: &[(&str, &str)] = &[
        ("azorius", "WU"), ("dimir", "UB"), ("rakdos", "BR"), ("gruul", "RG"), ("selesnya", "WG"), ("orzhov", "WB"), ("izzet", "UR"), ("golgari", "BG"), ("boros", "WR"), ("simic", "UG"),
        ("esper", "WUB"), ("grixis", "UBR"), ("jund", "BRG"), ("naya", "WRG"), ("bant", "WUG"), ("abzan", "WBG"), ("jeskai", "WUR"), ("sultai", "UBG"), ("mardu", "WBR"), ("temur", "URG"),
        ("mono white", "W"), ("mono-white", "W"), ("mono blue", "U"), ("mono-blue", "U"), ("mono black", "B"), ("mono-black", "B"), ("mono red", "R"), ("mono-red", "R"), ("mono green", "G"), ("mono-green", "G"),
        ("blue", "U"), ("white", "W"), ("black", "B"), ("red", "R"), ("green", "G"), ("4c", "WUBRG"), ("5c", "WUBRG"),
        ("colorless", "C"), ("colourless", "C"), ("c", "C"),
    ];
    let key = s.trim().to_lowercase();
    if let Some((_, l)) = NAMES.iter().find(|(n, _)| *n == key) {
        return l.to_string();
    }
    let up = s.trim().to_uppercase();
    if !up.is_empty() && up.chars().all(|c| "WUBRG".contains(c)) {
        return "WUBRG".chars().filter(|c| up.contains(*c)).collect();
    }
    s.trim().to_string()
}

/// WUBRG letters, or "C" for a colourless build.
fn is_colour_subtype(s: &str) -> bool {
    s == "C" || (!s.is_empty() && s.chars().all(|c| "WUBRG".contains(c)))
}

#[derive(Clone, Debug)]
struct Label {
    archetype: String,
    subtype: Option<String>,
    category: Option<String>,
}

impl Label {
    fn new(archetype: &str, subtype: Option<&str>, category: Option<String>) -> Label {
        Label { archetype: archetype.trim().to_string(), subtype: subtype.map(canon_subtype).filter(|s| !s.is_empty()), category }
    }
    fn show(&self) -> String {
        match &self.subtype {
            Some(s) => format!("{}/{s}", self.archetype),
            None => self.archetype.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct Anchor {
    doc: usize,
    label: Label,
    source: &'static str,
}

fn deck_label(d: &LdcDeck) -> Option<Label> {
    if d.ignore { None } else { Some(Label::new(&d.archetype, d.subtype.as_deref(), d.category.clone())) }
}

fn ldc_label(vocab: &Vocab, deck: &str) -> Option<Label> {
    match vocab.ldc.get(deck) {
        Some(d) => deck_label(d),
        None => Some(Label::new(deck, None, None)),
    }
}

fn skeleton_label(vocab: &Vocab, name: &str) -> Option<Label> {
    match vocab.goldfish.get(name).or_else(|| vocab.ldc.get(name)) {
        Some(d) => deck_label(d),
        None => Some(Label::new(name, None, None)),
    }
}

fn hand_label(vocab: &Vocab, name: &str) -> Option<Label> {
    let bare = name.split(" (").next().unwrap_or(name).trim();
    if let Some(al) = vocab.alias.get(bare) {
        return Some(Label::new(&al.archetype, al.subtype.as_deref(), None));
    }
    let (a, s) = crate::game::parse_deck_name(name);
    if a.is_empty() || a == "Unknown" {
        return None;
    }
    Some(Label::new(a, s, None))
}

/// Anchors per (match, side). Precedence when several sources know the same match:
/// manual > LDC event sheet > registered list > hand entry.
fn collect_anchors(conn: &mut SqliteConnection, doc_ix: &HashMap<(i32, String), usize>, vocab: &Vocab, rules: &[super::label::ArchetypeRule], username: &str) -> QueryResult<Vec<Anchor>> {
    #[derive(QueryableByName)]
    struct LdcRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        match_id: i32,
        #[diesel(sql_type = Nullable<Text>)]
        opp_deck: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        my_deck: Option<String>,
    }
    #[derive(QueryableByName)]
    struct HandRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        match_id: i32,
        #[diesel(sql_type = Text)]
        deck_name: String,
        #[diesel(sql_type = Text)]
        opponent_deck: String,
    }
    #[derive(QueryableByName)]
    struct LabelRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        match_id: i32,
        #[diesel(sql_type = Text)]
        side: String,
        #[diesel(sql_type = Text)]
        source: String,
        #[diesel(sql_type = Text)]
        archetype: String,
        #[diesel(sql_type = Nullable<Text>)]
        subtype: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        category: Option<String>,
    }
    #[derive(QueryableByName)]
    struct CatRow {
        #[diesel(sql_type = Text)]
        archetype: String,
        #[diesel(sql_type = Nullable<Text>)]
        subtype: Option<String>,
        #[diesel(sql_type = Text)]
        category: String,
    }
    let mut best: HashMap<(i32, &'static str), (u8, Label, &'static str)> = HashMap::new();
    let mut put = |mid: i32, side: &'static str, prio: u8, label: Label, source: &'static str| {
        let e = best.entry((mid, side)).or_insert((0, label.clone(), source));
        if prio > e.0 || e.0 == 0 {
            *e = (prio, label, source);
        }
    };
    let has_hand: i64 = diesel::sql_query("SELECT COUNT(*) AS n FROM sqlite_master WHERE type = 'table' AND name = 'matches'").get_result::<super::db::CountRow>(conn)?.n;
    let hand: Vec<HandRow> = if has_hand == 0 {
        Vec::new()
    } else {
        diesel::sql_query(
            "SELECT m.id AS match_id, h.deck_name, h.opponent_deck FROM matches h
             JOIN mtgo_matches m ON substr(m.start_ts,1,10) = h.date AND lower(m.opponent) = lower(h.opponent_name) AND m.replay = 0",
        )
        .load(conn)?
    };
    for r in hand {
        if let Some(l) = hand_label(vocab, &r.deck_name) { put(r.match_id, "me", 1, l, "hand"); }
        if let Some(l) = hand_label(vocab, &r.opponent_deck) { put(r.match_id, "opp", 1, l, "hand"); }
    }
    let rows: Vec<LabelRow> = diesel::sql_query("SELECT match_id, side, source, archetype, subtype, category FROM mtgo_match_labels WHERE source IN ('list','manual') AND archetype IS NOT NULL").load(conn)?;
    for r in rows {
        let side: &'static str = if r.side == "me" { "me" } else { "opp" };
        let (prio, src): (u8, &'static str) = if r.source == "manual" { (4, "manual") } else { (2, "list") };
        put(r.match_id, side, prio, Label::new(&r.archetype, r.subtype.as_deref(), r.category), src);
    }
    if !username.is_empty() {
        let rows: Vec<LdcRow> = diesel::sql_query(
            "SELECT m.id AS match_id, p.deck AS opp_deck, me.deck AS my_deck FROM mtgo_matches m
             JOIN ldc_players p ON p.date = substr(m.start_ts,1,10) AND lower(p.player) = lower(m.opponent)
             JOIN ldc_players me ON me.event = p.event AND lower(me.player) = lower(?)
             WHERE m.replay = 0",
        )
        .bind::<Text, _>(username)
        .load(conn)?;
        for r in rows {
            if let Some(l) = r.opp_deck.as_deref().and_then(|d| ldc_label(vocab, d)) { put(r.match_id, "opp", 3, l, "ldc"); }
            if let Some(l) = r.my_deck.as_deref().and_then(|d| ldc_label(vocab, d)) { put(r.match_id, "me", 3, l, "ldc"); }
        }
    }
    // Categories: the hand-entered taxonomy when present, else the marker rules (web DBs have no deck_types).
    let has_types: i64 = diesel::sql_query("SELECT COUNT(*) AS n FROM sqlite_master WHERE type = 'table' AND name = 'deck_types'").get_result::<super::db::CountRow>(conn)?.n;
    let cats: Vec<CatRow> = if has_types == 0 { Vec::new() } else { diesel::sql_query("SELECT archetype, subtype, category FROM deck_types").load(conn)? };
    let mut cat: HashMap<(String, Option<String>), String> = cats.into_iter().map(|c| ((c.archetype, c.subtype), c.category)).collect();
    for r in rules {
        cat.entry((r.name.clone(), None)).or_insert_with(|| r.category.clone());
    }
    let mut out = Vec::new();
    for ((mid, side), (_, mut label, source)) in best {
        let Some(&doc) = doc_ix.get(&(mid, side.to_string())) else { continue };
        label.category = cat.get(&(label.archetype.clone(), label.subtype.clone())).or_else(|| cat.get(&(label.archetype.clone(), None))).cloned().or(label.category);
        out.push(Anchor { doc, label, source });
    }
    Ok(out)
}

struct Skel {
    name: String,
    label: Label,
    /// metagame share (per cent), the prior among colour variants of one shell
    share: f32,
    vec: Vec<(u32, f32)>,
}

fn load_skeletons(conn: &mut SqliteConnection, v: &Vectors, vocab: &Vocab) -> QueryResult<Vec<Skel>> {
    Ok(super::skeleton::load(conn)?
        .into_iter()
        .filter_map(|s| skeleton_label(vocab, &s.name).map(|label| Skel { name: s.name.clone(), label, share: s.share as f32, vec: v.vector_for_weights(&s.cards) }))
        .collect())
}

/// Everything the labeller concluded about one document, for writing or for showing.
struct Verdict {
    label: Option<Label>,
    /// short evidence tag stored in archetype_marker
    marker: String,
    sub_marker: Option<String>,
    /// the long form for --explain
    detail: String,
    /// worth a human look: unknown, skeleton-labelled, or a thin vote
    uncertain: bool,
    /// the archetype that led the vote (for --check), whether or not it was labelled
    best: Option<String>,
    margin: f32,
    /// distinct candidate labels with their best similarity, nearest first (for review)
    guesses: Vec<(Label, f32)>,
}

fn distinct_guesses<'a>(it: impl Iterator<Item = (&'a Label, f32)>) -> Vec<(Label, f32)> {
    let mut out: Vec<(Label, f32)> = Vec::new();
    for (l, s) in it {
        if !out.iter().any(|(o, _)| o.show() == l.show()) {
            out.push((l.clone(), s));
        }
    }
    out
}

struct Params {
    k: usize,
    margin: f32,
    min_sim: f32,
    skel_min_sim: f32,
}

/// Similarity-weighted vote of the k nearest anchors in `pool` for document `i`; None when the
/// nearest anchor is below the similarity floor.
fn vote(v: &Vectors, i: usize, pool: &[&Anchor], skels: &[Skel], proven: &str, p: &Params) -> Option<Verdict> {
    let mut sims: Vec<(f32, &Anchor)> = pool.iter().map(|a| (dot(&v.vecs[i], &v.vecs[a.doc]), *a)).filter(|(s, a)| a.label.archetype != "Other" || *s >= OTHER_ANCHOR_MIN_SIM).collect();
    sims.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    sims.truncate(p.k);
    let top_sim = sims.first().map(|s| s.0).unwrap_or(0.0);
    if top_sim < p.min_sim {
        return None;
    }
    let near = sims.iter().map(|(s, a)| format!("{} {s:.2} {}", a.label.show(), a.source)).collect::<Vec<_>>().join(" | ");
    let guesses = distinct_guesses(sims.iter().map(|(s, a)| (&a.label, *s)));
    let mut by_arch: BTreeMap<&str, (f32, usize)> = BTreeMap::new();
    for (s, a) in &sims {
        if *s > 0.0 {
            let e = by_arch.entry(&a.label.archetype).or_default();
            e.0 += s.powf(POWER);
            e.1 += 1;
        }
    }
    let mut ranked: Vec<(&str, f32, usize)> = by_arch.iter().map(|(a, (w, n))| (*a, *w, *n)).collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let (best, bw, bn) = ranked[0];
    let second = ranked.get(1).map(|r| r.1).unwrap_or(0.0);
    let m = (bw - second) / bw;
    let cast = sims.iter().filter(|s| s.0 > 0.0).count();
    if m < p.margin {
        return Some(Verdict { label: None, marker: format!("knn? {best} {bn}/{cast} s{top_sim:.2} m{m:.2}"), sub_marker: None, detail: format!("nearest: {near}"), uncertain: true, best: Some(best.to_string()), margin: m, guesses });
    }
    let mut by_sub: BTreeMap<&str, (f32, usize)> = BTreeMap::new();
    let mut category = None;
    for (s, a) in &sims {
        if *s > 0.0 && a.label.archetype == best {
            category = category.or(a.label.category.clone());
            if let Some(st) = &a.label.subtype {
                let e = by_sub.entry(st).or_default();
                e.0 += s.powf(POWER);
                e.1 += 1;
            }
        }
    }
    let (subtype, sub_marker) = decide_subtype(&by_sub, bn, skels, best, proven);
    Some(Verdict {
        label: Some(Label { archetype: best.to_string(), subtype, category }),
        marker: format!("knn {bn}/{cast} s{top_sim:.2} m{m:.2}"),
        sub_marker,
        detail: format!("vote {bn}/{cast} m{m:.2}  nearest: {near}"),
        uncertain: m < UNCERTAIN_MARGIN,
        best: Some(best.to_string()),
        margin: m,
        guesses,
    })
}

/// Nearest published skeleton, used only when no observed anchor is near.
/// Subtype of the winning shell. Build words (Turbo, Yorion, Mystic Forge) are voted: a majority
/// of the shell's voters must name one, voters that know no subtype counting against. Colour
/// variants are not voted — the neighbours are near because of the cards the variants share, so
/// their colour says nothing about this match. A colour variant must contain the colours this
/// match proved ("C" only fits a match that proved none), and among the consistent ones the
/// field share decides: the most common deck the cards allow. With nothing proven the winner
/// must also hold a majority of the share, or the variant stays open.
fn decide_subtype(by_sub: &BTreeMap<&str, (f32, usize)>, voters: usize, skels: &[Skel], archetype: &str, proven: &str) -> (Option<String>, Option<String>) {
    let word = by_sub.iter().filter(|(s, _)| !is_colour_subtype(s)).max_by(|a, b| a.1 .0.partial_cmp(&b.1 .0).unwrap()).map(|(s, (_, n))| (s.to_string(), *n));
    if let Some((s, n)) = &word {
        if n * 2 > voters {
            return (Some(s.clone()), Some(format!("knn {n}/{voters}")));
        }
    }
    let mut variants: BTreeMap<String, (f32, f32)> = BTreeMap::new(); // subtype → (share, vote mass)
    for s in skels.iter().filter(|s| s.label.archetype == archetype) {
        if let Some(st) = &s.label.subtype {
            if is_colour_subtype(st) { variants.entry(st.clone()).or_default().0 += s.share; }
        }
    }
    for (st, (w, _)) in by_sub {
        if is_colour_subtype(st) { variants.entry(st.to_string()).or_default().1 += w; }
    }
    if variants.is_empty() {
        return match word { Some((_, n)) => (None, Some(format!("knn {n}/{voters}"))), None => (None, None) };
    }
    let shown = if proven.is_empty() { "∅" } else { proven };
    let consistent: Vec<(&String, f32)> = {
        let any_share = variants.values().any(|(s, _)| *s > 0.0);
        variants.iter().filter(|(st, _)| if *st == "C" { proven.is_empty() } else { proven.chars().all(|c| st.contains(c)) }).map(|(st, (s, w))| (st, if any_share { *s } else { *w })).collect()
    };
    let total: f32 = consistent.iter().map(|(_, x)| x).sum();
    let best = consistent.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    match best {
        Some((st, _)) if consistent.len() == 1 => (Some((*st).clone()), Some(format!("only variant ⊇ {shown}"))),
        Some((st, score)) if !proven.is_empty() || *score * 2.0 > total => (Some((*st).clone()), Some(format!("share {:.0}% ⊇ {shown}", if total > 0.0 { 100.0 * score / total } else { 0.0 }))),
        Some(_) => (None, Some(format!("no majority variant ⊇ {shown}"))),
        None if !proven.is_empty() => (Some(proven.to_string()), Some("proven, no known variant".into())),
        None => (None, None),
    }
}

fn skeleton_vote(v: &Vectors, i: usize, skels: &[Skel], p: &Params) -> Verdict {
    let mut sims: Vec<(f32, &Skel)> = skels.iter().map(|s| (dot(&v.vecs[i], &s.vec), s)).collect();
    sims.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let Some(&(top, best)) = sims.first() else {
        return Verdict { label: None, marker: format!("knn? nearest anchor below s{:.2}", p.min_sim), sub_marker: None, detail: String::new(), uncertain: true, best: None, margin: 0.0, guesses: vec![] };
    };
    let guesses = distinct_guesses(sims.iter().take(3).map(|(s, k)| (&k.label, *s)));
    let second = sims.get(1).map(|s| s.0).unwrap_or(0.0);
    let m = if top > 0.0 { (top - second) / top } else { 0.0 };
    let next = sims.iter().skip(1).take(2).map(|(s, k)| format!("{} {s:.2}", k.name)).collect::<Vec<_>>().join(", ");
    if top >= p.skel_min_sim && m >= p.margin {
        Verdict { label: Some(best.label.clone()), marker: format!("skeleton:{} s{top:.2} m{m:.2}", best.name), sub_marker: best.label.subtype.as_ref().map(|_| "skeleton".to_string()), detail: format!("no anchor within s{:.2}; skeleton {} s{top:.2} m{m:.2} (next: {next})", p.min_sim, best.name), uncertain: true, best: Some(best.label.archetype.clone()), margin: m, guesses }
    } else {
        Verdict { label: None, marker: format!("knn? nearest anchor below s{:.2}; skeleton? {} s{top:.2} m{m:.2}", p.min_sim, best.name), sub_marker: None, detail: format!("no anchor within s{:.2}; nearest skeleton {} s{top:.2} m{m:.2} (next: {next}) — below floor or margin", p.min_sim, best.name), uncertain: true, best: Some(best.label.archetype.clone()), margin: m, guesses }
    }
}

fn classify(v: &Vectors, i: usize, n_cards: usize, pool: &[&Anchor], skels: &[Skel], proven: &str, p: &Params) -> Verdict {
    if let Some(vt) = vote(v, i, pool, skels, proven, p) {
        return vt;
    }
    let sk = skeleton_vote(v, i, skels, p);
    if sk.label.is_some() || n_cards < MIN_CARDS_FOR_OTHER {
        return sk;
    }
    // well observed and like nothing known: off-meta, with whatever colours it proved
    Verdict {
        label: Some(Label::new("Other", if proven.is_empty() { None } else { Some(proven) }, Some("Other".into()))),
        marker: format!("other: {n_cards} cards, {}", sk.marker.trim_start_matches("knn? ")),
        sub_marker: if proven.is_empty() { None } else { Some("proven".into()) },
        detail: format!("off-meta: {n_cards} distinct cards seen; {}", sk.detail),
        uncertain: true,
        best: Some("Other".into()),
        margin: 0.0,
        guesses: vec![],
    }
}

/// Proven colours per (match_id, side) from mtgo_match_colors.
fn load_colours(conn: &mut SqliteConnection) -> QueryResult<HashMap<(i32, String), String>> {
    #[derive(QueryableByName)]
    struct C {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        match_id: i32,
        #[diesel(sql_type = Text)]
        side: String,
        #[diesel(sql_type = Text)]
        colors: String,
    }
    Ok(diesel::sql_query("SELECT match_id, side, colors FROM mtgo_match_colors").load::<C>(conn)?.into_iter().map(|c| ((c.match_id, c.side), c.colors)).collect())
}

fn state_get(conn: &mut SqliteConnection, key: &str) -> QueryResult<Option<String>> {
    #[derive(QueryableByName)]
    struct V {
        #[diesel(sql_type = Text)]
        value: String,
    }
    let v: Vec<V> = diesel::sql_query("SELECT value FROM mtgo_state WHERE key = ?").bind::<Text, _>(key).load(conn)?;
    Ok(v.into_iter().next().map(|v| v.value))
}

fn state_set(conn: &mut SqliteConnection, key: &str, value: &str) -> QueryResult<()> {
    diesel::sql_query("INSERT OR REPLACE INTO mtgo_state(key, value) VALUES (?, ?)").bind::<Text, _>(key).bind::<Text, _>(value).execute(conn)?;
    Ok(())
}

fn day_number(d: &str) -> i32 {
    let p: Vec<i32> = d.split('-').filter_map(|x| x.parse().ok()).collect();
    if p.len() == 3 { p[0] * 372 + p[1] * 31 + p[2] } else { 0 }
}

/// Everything loaded once per invocation: signatures, vectors, anchors, skeletons.
struct Model {
    docs: Vec<Doc>,
    v: Vectors,
    anchors: Vec<Anchor>,
    skels: Vec<Skel>,
    rules: Vec<super::label::ArchetypeRule>,
}

impl Model {
    fn load(conn: &mut SqliteConnection, ldc_file: &Path, markers: &Path, username: &str, quiet: bool) -> Result<Model, String> {
        super::db::ensure_schema(conn).map_err(|e| e.to_string())?;
        let vocab = load_ldc_map(ldc_file)?;
        let rules = super::label::load_markers(markers)?;
        if username.is_empty() {
            eprintln!("warning: [mtgo].username is not set in config.toml; LDC anchors need it to find the events I played");
        }
        let (docs, _) = load_docs(conn, false).map_err(|e| e.to_string())?;
        let v = vectorize_with(&docs, true);
        let doc_ix: HashMap<(i32, String), usize> = docs.iter().enumerate().map(|(i, d)| ((d.match_id, d.side.clone()), i)).collect();
        let anchors = collect_anchors(conn, &doc_ix, &vocab, &rules, username).map_err(|e| e.to_string())?;
        let skels = load_skeletons(conn, &v, &vocab).map_err(|e| e.to_string())?;
        if !quiet {
            let mut by_source: BTreeMap<&str, usize> = BTreeMap::new();
            for a in &anchors {
                *by_source.entry(a.source).or_default() += 1;
            }
            println!("anchors: {} ({}); skeletons: {}", anchors.len(), by_source.iter().map(|(s, n)| format!("{s} {n}")).collect::<Vec<_>>().join(", "), skels.len());
        }
        Ok(Model { docs, v, anchors, skels, rules })
    }
    fn anchor_of(&self) -> HashMap<usize, &Anchor> {
        self.anchors.iter().map(|a| (a.doc, a)).collect()
    }
    fn pool(&self) -> Vec<&Anchor> {
        self.anchors.iter().filter(|a| self.docs[a.doc].side == "opp").collect()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(conn: &mut SqliteConnection, ldc_file: &Path, markers: &Path, username: &str, k: usize, margin: f32, min_sim: f32, check: bool, since: Option<&str>, explain_since: Option<&str>, uncertain_only: bool) -> Result<(), String> {
    let model = Model::load(conn, ldc_file, markers, username, false)?;
    let Model { docs, v, skels, .. } = &model;
    let anchor_of = model.anchor_of();
    let pool = model.pool();
    let params = Params { k, margin, min_sim, skel_min_sim: 0.30 };

    if check {
        return loo_check(conn, docs, v, &pool, skels, &params, since).map_err(|e| e.to_string());
    }
    if let Some(since) = explain_since {
        let since = if since == "last" {
            match state_get(conn, "explain_reviewed_through").map_err(|e| e.to_string())? {
                Some(d) => d,
                None => return Err("no review yet: give a date the first time, e.g. `mtgo label --explain 2026-08-10 --uncertain`".into()),
            }
        } else {
            since.to_string()
        };
        explain(conn, docs, v, &anchor_of, &pool, skels, &params, &since, uncertain_only).map_err(|e| e.to_string())?;
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        state_set(conn, "explain_reviewed_through", &today).map_err(|e| e.to_string())?;
        println!("reviewed through {today}; the next `mtgo label --explain` (no date) starts there");
        return Ok(());
    }
    let (counts, unknown, by_skeleton) = label_all(conn, &model, &params)?;
    print_label_summary(conn, &counts, unknown, by_skeleton, margin, min_sim, &params)
}

/// Write every opponent label (my side: marker rules). Returns (counts by label, unknown, skeleton-labelled).
fn label_all(conn: &mut SqliteConnection, model: &Model, params: &Params) -> Result<(BTreeMap<(String, String), usize>, usize, usize), String> {
    let Model { docs, v, skels, rules, .. } = model;
    let anchor_of = model.anchor_of();
    let pool = model.pool();
    super::label::run_label_sides(conn, rules, &["me"]).map_err(|e| e.to_string())?;
    let coloured = super::colors::compute(conn).map_err(|e| e.to_string())?;
    if coloured == 0 {
        eprintln!("note: no card colour facts loaded (`mtgo cards-import`), colour variants fall back to anchors");
    }
    let colours = load_colours(conn).map_err(|e| e.to_string())?;
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    let (mut unknown, mut by_skeleton) = (0usize, 0usize);
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        diesel::sql_query("DELETE FROM mtgo_match_labels WHERE source = 'guess' AND side = 'opp'").execute(conn)?;
        for (i, d) in docs.iter().enumerate().filter(|(_, d)| d.side == "opp") {
            let proven = colours.get(&(d.match_id, "opp".to_string())).map(String::as_str).unwrap_or("");
            let verdict = match anchor_of.get(&i) {
                Some(a) => {
                    // an anchor names the shell; if it names no variant, the proven colours may
                    let mut label = a.label.clone();
                    let mut sub_marker = label.subtype.as_ref().map(|_| format!("anchor:{}", a.source));
                    if label.subtype.is_none() {
                        let (st, why) = decide_subtype(&BTreeMap::new(), 0, skels, &label.archetype, proven);
                        label.subtype = st;
                        sub_marker = why;
                    }
                    Verdict { label: Some(label), marker: format!("anchor:{}", a.source), sub_marker, detail: String::new(), uncertain: false, best: None, margin: 1.0, guesses: vec![] }
                }
                None => classify(v, i, d.cards.len(), &pool.iter().copied().filter(|a| a.doc != i).collect::<Vec<_>>(), skels, proven, params),
            };
            if verdict.marker.starts_with("skeleton:") {
                by_skeleton += 1;
            }
            let row = match &verdict.label {
                Some(l) => {
                    *counts.entry((l.archetype.clone(), l.subtype.clone().unwrap_or("(unknown)".into()))).or_default() += 1;
                    NewLabel { match_id: d.match_id, side: "opp", source: "guess", archetype: Some(l.archetype.clone()), subtype: l.subtype.clone(), category: l.category.clone(), archetype_marker: Some(verdict.marker.clone()), subtype_marker: verdict.sub_marker.clone() }
                }
                None => {
                    unknown += 1;
                    NewLabel { match_id: d.match_id, side: "opp", source: "guess", archetype: None, subtype: None, category: None, archetype_marker: Some(verdict.marker.clone()), subtype_marker: None }
                }
            };
            diesel::insert_into(mtgo_match_labels::table).values(row).execute(conn)?;
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    super::stats::ensure_view(conn).map_err(|e| e.to_string())?;
    Ok((counts, unknown, by_skeleton))
}

fn print_label_summary(_conn: &mut SqliteConnection, counts: &BTreeMap<(String, String), usize>, unknown: usize, by_skeleton: usize, margin: f32, min_sim: f32, params: &Params) -> Result<(), String> {
    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED);
    t.set_header(["opp: archetype", "subtype", "matches"]);
    let mut rows: Vec<_> = counts.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for ((a, s), n) in rows.iter().take(30) {
        t.add_row(vec![a.clone(), s.clone(), n.to_string()]);
    }
    println!("{t}");
    println!("opp: {unknown} matches unknown (vote margin < {margin:.2}, or nothing within s{min_sim:.2} and no skeleton within s{:.2}); {by_skeleton} labelled from skeletons only", params.skel_min_sim);
    println!("opponent labels → mtgo_match_labels(source = guess); marker = 'anchor:<source>' | 'knn <votes>/<cast> s<top similarity> m<margin>' | 'skeleton:<archetype> …'. My side: marker rules. `mtgo review` to validate, `--check` for leave-one-out accuracy.");
    Ok(())
}

/// Interactive validation: each uncertain opponent match since the last review, three lines of
/// context, then the deck typeahead (the model's guesses first). Enter anchors it, `skip` moves on,
/// `quit` stops; labels are rebuilt at the end and the review date advances to the last match shown.
pub fn review(conn: &mut SqliteConnection, ldc_file: &Path, markers: &Path, username: &str, since: Option<&str>) -> Result<(), String> {
    #[derive(QueryableByName)]
    struct C {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        match_id: i32,
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        played: bool,
    }
    #[derive(QueryableByName)]
    struct T {
        #[diesel(sql_type = Text)]
        name: String,
    }
    #[derive(QueryableByName)]
    struct T2 {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        match_id: i32,
        #[diesel(sql_type = Text)]
        name: String,
    }
    let since = match since {
        Some(s) => s.to_string(),
        None => state_get(conn, "explain_reviewed_through").map_err(|e| e.to_string())?.ok_or("first review: give a start date, e.g. `mtgo review --since 2026-08-10`")?,
    };
    let model = Model::load(conn, ldc_file, markers, username, true)?;
    let params = Params { k: 5, margin: 0.3, min_sim: 0.35, skel_min_sim: 0.30 };
    let anchor_of = model.anchor_of();
    let pool = model.pool();
    let all_colours = load_colours(conn).map_err(|e| e.to_string())?;
    let doc_ix: HashMap<i32, usize> = model.docs.iter().enumerate().filter(|(_, d)| d.side == "opp").map(|(i, d)| (d.match_id, i)).collect();
    let mut todo: Vec<(MatchMeta, Verdict)> = Vec::new();
    for m in match_meta(conn, &since).map_err(|e| e.to_string())? {
        let Some(&i) = doc_ix.get(&m.id) else { continue };
        if anchor_of.contains_key(&i) { continue; }
        let proven = all_colours.get(&(m.id, "opp".to_string())).map(String::as_str).unwrap_or("");
        let verdict = classify(&model.v, i, model.docs[i].cards.len(), &pool.iter().copied().filter(|a| a.doc != i).collect::<Vec<_>>(), &model.skels, proven, &params);
        if verdict.uncertain { todo.push((m, verdict)); }
    }
    if todo.is_empty() {
        println!("nothing to review since {since}");
        return Ok(());
    }
    // deck vocabulary for the typeahead: deck_types + LDC map + anchors, as "Archetype: Subtype"
    let mut vocab: std::collections::BTreeSet<String> = diesel::sql_query("SELECT archetype || COALESCE(': ' || subtype, '') AS name FROM deck_types").load::<T>(conn).map_err(|e| e.to_string())?.into_iter().map(|t| {
        let (a, s) = crate::game::parse_deck_name(&t.name);
        match s.map(canon_subtype).filter(|s| !s.is_empty()) { Some(s) => format!("{a}: {s}"), None => a.to_string() }
    }).collect();
    for a in &model.anchors {
        vocab.insert(match &a.label.subtype { Some(s) => format!("{}: {s}", a.label.archetype), None => a.label.archetype.clone() });
    }
    let cards: Vec<C> = diesel::sql_query("SELECT match_id, name, MAX(played) AS played FROM mtgo_game_cards WHERE side = 'opp' AND name NOT IN ('Island','Swamp','Mountain','Forest','Plains') AND name NOT LIKE '%Token' GROUP BY match_id, name ORDER BY match_id, MAX(played) DESC, SUM(plays) DESC").load(conn).map_err(|e| e.to_string())?;
    let mut by_match: HashMap<i32, Vec<(String, bool)>> = HashMap::new();
    for c in cards {
        by_match.entry(c.match_id).or_default().push((c.name, c.played));
    }
    let colours: HashMap<i32, String> = diesel::sql_query("SELECT match_id, colors AS name FROM mtgo_match_colors WHERE side = 'opp'").load::<T2>(conn).map_err(|e| e.to_string())?.into_iter().map(|t| (t.match_id, t.name)).collect();
    let total = todo.len();
    let (mut anchored, mut skipped, mut last_day) = (0usize, 0usize, since.clone());
    println!("{total} matches to review since {since}. Enter = take the highlighted deck, type to search, `new deck name…` to name one, `skip`, `quit` (Esc asks first).\n");
    for (n, (m, verdict)) in todo.iter().enumerate() {
        let cs = by_match.get(&m.id).cloned().unwrap_or_default();
        let played: Vec<&str> = cs.iter().filter(|c| c.1).map(|c| c.0.as_str()).collect();
        let seen: Vec<&str> = cs.iter().filter(|c| !c.1).map(|c| c.0.as_str()).collect();
        let shown = played.iter().chain(seen.iter()).take(14).cloned().collect::<Vec<_>>().join(", ");
        let more = cs.len().saturating_sub(14);
        let model_line = if verdict.guesses.is_empty() { "nothing similar in anchors or skeletons".to_string() } else { verdict.guesses.iter().take(3).map(|(l, s)| format!("{} {s:.2}", l.show())).collect::<Vec<_>>().join(" · ") };
        let col = colours.get(&m.id).map(|c| if c.is_empty() { "colours: none proven".to_string() } else { format!("colours: {c}") }).unwrap_or_default();
        let header = format!(
            "[{}/{total}] #{} {} vs {} [{}]  {col}\n{shown}{}\nmodel: {}{model_line}",
            n + 1, m.id, m.day, m.opponent.clone().unwrap_or_default(), m.result,
            if more > 0 { format!("  +{more} more") } else { String::new() },
            if verdict.marker.starts_with("skeleton") { "(skeleton) " } else { "" }
        );
        println!("{header}");
        let mut options: Vec<String> = verdict.guesses.iter().take(3).map(|(l, _)| match &l.subtype { Some(s) => format!("{}: {s}", l.archetype), None => l.archetype.clone() }).collect();
        if let Some(l) = verdict.label.as_ref().filter(|l| l.archetype == "Other") {
            options.push(l.show().replace('/', ": "));
        }
        options.push("new deck name…".into());
        options.push("skip".into());
        options.push("quit".into());
        let extra: Vec<String> = vocab.iter().filter(|x| !options.contains(x)).cloned().collect();
        options.extend(extra);
        last_day = m.day.clone();
        // one match: loop until a real decision (Esc / an empty Tab asks before quitting)
        let mut quit = false;
        loop {
            let choice = crate::deck::fuzzy_select_framed("Deck", &header, &options);
            let pick: String = match choice.as_deref() {
                None => {
                    if dialoguer::Confirm::new().with_prompt("Quit the review? (progress is kept)").default(false).interact().unwrap_or(false) { quit = true; break; }
                    continue;
                }
                Some("quit") => { quit = true; break; }
                Some("skip") => { skipped += 1; break; }
                Some("new deck name…") => {
                    let typed: String = dialoguer::Input::new().with_prompt("Deck name (Archetype or Archetype: Subtype; colours as letters)").allow_empty(true).interact_text().unwrap_or_default();
                    if typed.trim().is_empty() { continue; }
                    typed.trim().to_string()
                }
                Some(p) => p.to_string(),
            };
            let (a, s) = crate::game::parse_deck_name(&pick);
            let s = s.map(canon_subtype);
            let shown = match &s { Some(s) => format!("{a}: {s}"), None => a.to_string() };
            match super::label::label_match(conn, &model.rules, m.id, "opp", a, s.as_deref(), None) {
                Ok(()) => { anchored += 1; println!("  ✓ #{} → {shown}\n", m.id); }
                Err(e) => println!("  ✗ {e}\n"),
            }
            break;
        }
        if quit { break; }
    }
    state_set(conn, "explain_reviewed_through", &last_day).map_err(|e| e.to_string())?;
    if anchored > 0 {
        let model = Model::load(conn, ldc_file, markers, username, true)?;
        let (_, unknown, by_skeleton) = label_all(conn, &model, &params)?;
        println!("anchored {anchored}, skipped {skipped}; labels rebuilt ({unknown} opponents still unknown, {by_skeleton} on skeletons only). `mtgo label --check --since {since}` scores today's anchors.");
    } else {
        println!("anchored 0, skipped {skipped}; labels unchanged.");
    }
    println!("reviewed through {last_day}; next `mtgo review` starts there (skipped matches are not shown again unless you pass --since).");
    Ok(())
}

#[derive(QueryableByName)]
struct MatchMeta {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = Text)]
    day: String,
    #[diesel(sql_type = Nullable<Text>)]
    opponent: Option<String>,
    #[diesel(sql_type = Text)]
    result: String,
}

fn match_meta(conn: &mut SqliteConnection, since: &str) -> QueryResult<Vec<MatchMeta>> {
    diesel::sql_query("SELECT id, substr(start_ts,1,10) AS day, opponent, result FROM mtgo_matches WHERE replay = 0 AND start_ts IS NOT NULL AND start_ts >= ? ORDER BY start_ts").bind::<Text, _>(since).load(conn)
}

/// For validation by eye: every opponent match since `since` with its verdict, evidence and the
/// cards it showed (played first, then merely seen). Nothing is written.
#[allow(clippy::too_many_arguments)]
fn explain(conn: &mut SqliteConnection, docs: &[Doc], v: &Vectors, anchor_of: &HashMap<usize, &Anchor>, pool: &[&Anchor], skels: &[Skel], p: &Params, since: &str, uncertain_only: bool) -> QueryResult<()> {
    #[derive(QueryableByName)]
    struct C {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        match_id: i32,
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        played: bool,
    }
    let ms = match_meta(conn, since)?;
    let cards: Vec<C> = diesel::sql_query(
        "SELECT match_id, name, MAX(played) AS played FROM mtgo_game_cards WHERE side = 'opp' AND name NOT IN ('Island','Swamp','Mountain','Forest','Plains') AND name NOT LIKE '%Token' GROUP BY match_id, name ORDER BY match_id, MAX(played) DESC, SUM(plays) DESC",
    )
    .load(conn)?;
    let mut by_match: HashMap<i32, Vec<(String, bool)>> = HashMap::new();
    for c in cards {
        by_match.entry(c.match_id).or_default().push((c.name, c.played));
    }
    let doc_ix: HashMap<i32, usize> = docs.iter().enumerate().filter(|(_, d)| d.side == "opp").map(|(i, d)| (d.match_id, i)).collect();
    #[derive(QueryableByName)]
    struct Col {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        match_id: i32,
        #[diesel(sql_type = Text)]
        colors: String,
    }
    let colours: HashMap<i32, String> = diesel::sql_query("SELECT match_id, colors FROM mtgo_match_colors WHERE side = 'opp'").load::<Col>(conn)?.into_iter().map(|c| (c.match_id, c.colors)).collect();
    let (mut shown, mut total) = (0, 0);
    for m in ms {
        let Some(&i) = doc_ix.get(&m.id) else { continue };
        total += 1;
        let verdict = match anchor_of.get(&i) {
            Some(a) => Verdict { label: Some(a.label.clone()), marker: format!("anchor:{}", a.source), sub_marker: None, detail: format!("anchor:{}", a.source), uncertain: false, best: None, margin: 1.0, guesses: vec![] },
            None => classify(v, i, docs[i].cards.len(), &pool.iter().copied().filter(|a| a.doc != i).collect::<Vec<_>>(), skels, colours.get(&m.id).map(String::as_str).unwrap_or(""), p),
        };
        if uncertain_only && !verdict.uncertain {
            continue;
        }
        shown += 1;
        let head = match &verdict.label {
            Some(l) => l.show(),
            None => format!("UNKNOWN ({})", verdict.marker.trim_start_matches("knn? ")),
        };
        let cs = by_match.get(&m.id).cloned().unwrap_or_default();
        let played: Vec<&str> = cs.iter().filter(|c| c.1).map(|c| c.0.as_str()).collect();
        let seen: Vec<&str> = cs.iter().filter(|c| !c.1).map(|c| c.0.as_str()).collect();
        println!("#{} {} vs {} [{}] colours {} → {head}", m.id, m.day, m.opponent.unwrap_or_default(), m.result, colours.get(&m.id).filter(|c| !c.is_empty()).map(String::as_str).unwrap_or("none"));
        if !verdict.detail.is_empty() {
            println!("    {}", verdict.detail);
        }
        println!("    played: {}{}", played.join(", "), if seen.is_empty() { String::new() } else { format!("  | seen: {}", seen.join(", ")) });
    }
    if uncertain_only {
        println!("{shown} of {total} matches since {since} need a look (`mtgctl mtgo label-match <id> opp <Archetype> [--subtype X]` to anchor one)");
    }
    Ok(())
}

/// Leave-one-out over the anchors: each anchor is classified by the others (excluding, on the
/// opponent side, the same opponent within a day — near-duplicate signatures would flatter it).
/// Then the skeletons alone, by similarity band, to calibrate their floor.
fn loo_check(conn: &mut SqliteConnection, docs: &[Doc], v: &Vectors, pool: &[&Anchor], skels: &[Skel], p: &Params, since: Option<&str>) -> QueryResult<()> {
    let ms = match_meta(conn, "0000")?;
    let meta: HashMap<i32, (i32, String)> = ms.into_iter().map(|m| (m.id, (day_number(&m.day), m.opponent.unwrap_or_default().to_lowercase()))).collect();
    let colours = load_colours(conn)?;
    let (mut sub_n, mut sub_ok) = (0usize, 0usize);
    let since_day = since.map(day_number).unwrap_or(0);
    let tests: Vec<&&Anchor> = pool.iter().filter(|a| a.source != "list" && meta.get(&docs[a.doc].match_id).map(|m| m.0 >= since_day).unwrap_or(false)).collect();
    if tests.is_empty() {
        println!("no opponent anchors{}", since.map(|s| format!(" since {s}")).unwrap_or_default());
        return Ok(());
    }
    let mut results: Vec<(bool, f32, &Anchor)> = Vec::new();
    let mut skel_bands: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut skel_margin: (usize, usize) = (0, 0);
    for t in &tests {
        let (day, opp) = &meta[&docs[t.doc].match_id];
        let others: Vec<&Anchor> = pool
            .iter()
            .copied()
            .filter(|a| {
                if a.doc == t.doc { return false; }
                let (d2, o2) = &meta[&docs[a.doc].match_id];
                !(o2 == opp && (d2 - day).abs() <= 1)
            })
            .collect();
        let relaxed = Params { margin: 0.0, ..*p };
        let proven = colours.get(&(docs[t.doc].match_id, "opp".to_string())).map(String::as_str).unwrap_or("");
        match vote(v, t.doc, &others, skels, proven, &relaxed) {
            Some(vt) => {
                let ok = vt.best.as_deref() == Some(&t.label.archetype);
                if ok && vt.margin >= p.margin {
                    if let (Some(truth), Some(l)) = (&t.label.subtype, &vt.label) {
                        sub_n += 1;
                        sub_ok += (l.subtype.as_deref() == Some(truth.as_str())) as usize;
                    }
                }
                results.push((ok, vt.margin, t));
            }
            None => results.push((false, -1.0, t)),
        }
        // skeletons alone
        let sk = skeleton_vote(v, t.doc, skels, &Params { margin: 0.0, skel_min_sim: 0.0, ..*p });
        if let Some(best) = &sk.best {
            let top: f32 = sk.marker.split(" s").nth(1).and_then(|x| x.split(' ').next()).and_then(|x| x.parse().ok()).unwrap_or(0.0);
            let band = if top < 0.2 { "< 0.20" } else if top < 0.3 { "0.20–0.29" } else if top < 0.4 { "0.30–0.39" } else { "≥ 0.40" };
            let e = skel_bands.entry(band).or_default();
            e.0 += 1;
            e.1 += (best == &t.label.archetype) as usize;
            if top >= p.skel_min_sim && sk.margin >= p.margin {
                skel_margin.0 += 1;
                skel_margin.1 += (best == &t.label.archetype) as usize;
            }
        }
    }
    let n = results.len();
    println!("\n== opp: leave-one-out over {n} anchors{} (excluding registered lists), k = {}, similarity floor {:.2} (below it: unlabelled, counted as not covered)", since.map(|s| format!(" since {s}")).unwrap_or_default(), p.k, p.min_sim);
    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED);
    t.set_header(["margin ≥", "labelled", "coverage", "accuracy"]);
    for m in [0.0, 0.2, 0.3, 0.4, 0.5, 0.6] {
        let lab: Vec<_> = results.iter().filter(|r| r.1 >= m).collect();
        let ok = lab.iter().filter(|r| r.0).count();
        t.add_row(vec![format!("{m:.1}{}", if (m - p.margin).abs() < 1e-6 { " (current)" } else { "" }), lab.len().to_string(), format!("{:.0}%", 100.0 * lab.len() as f32 / n as f32), format!("{:.1}%", 100.0 * ok as f32 / lab.len().max(1) as f32)]);
    }
    println!("{t}");
    let mut by_src: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for r in &results {
        if r.1 >= p.margin {
            let e = by_src.entry(r.2.source).or_default();
            e.0 += 1;
            e.1 += r.0 as usize;
        }
    }
    println!("   at the current margin, by anchor source: {}", by_src.iter().map(|(s, (n, ok))| format!("{s} {ok}/{n}")).collect::<Vec<_>>().join(", "));
    println!("   subtype, where the archetype was right and the truth names one: {sub_ok}/{sub_n} ({:.1}%)", 100.0 * sub_ok as f32 / sub_n.max(1) as f32);
    let mut conf: BTreeMap<(String, String), usize> = BTreeMap::new();
    for r in &results {
        if r.1 >= p.margin && !r.0 {
            *conf.entry((r.2.label.archetype.clone(), String::new())).or_default() += 1;
        }
    }
    let mut conf: Vec<_> = conf.into_iter().collect();
    conf.sort_by(|a, b| b.1.cmp(&a.1));
    println!("   most-missed truths at the current margin: {}", conf.iter().take(8).map(|((a, _), n)| format!("{a} ×{n}")).collect::<Vec<_>>().join(", "));
    if !skels.is_empty() {
        println!("\n== skeletons alone (nearest published breakdown) on the same anchors, by similarity to the nearest skeleton:");
        for (band, (n, ok)) in &skel_bands {
            println!("   {band:10} n={n:3}  accuracy {:.1}%", 100.0 * *ok as f32 / (*n).max(1) as f32);
        }
        println!("   at the skeleton floor {:.2} and margin {:.2}: {}/{} correct ({:.1}%)", p.skel_min_sim, p.margin, skel_margin.1, skel_margin.0, 100.0 * skel_margin.1 as f32 / skel_margin.0.max(1) as f32);
    }
    Ok(())
}
