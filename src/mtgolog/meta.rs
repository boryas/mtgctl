//! The current metagame: the decks of the published Challenge lists (`ldc_lists`) over a recent
//! window, each with the cards its lists play, beside mtggoldfish's own Legacy metagame page.

use super::{ldc, reference};
use diesel::prelude::*;
use diesel::sql_types::{Nullable, Text};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Default)]
pub struct Card {
    pub name: String,
    /// lists playing it in the main deck, and the copies they play in total
    pub main_lists: usize,
    pub main_copies: i32,
    pub side_lists: usize,
    pub side_copies: i32,
}

#[derive(Serialize)]
pub struct Deck {
    /// The list's reference deck; else the name goldfish or the LDC gave it.
    pub deck: String,
    pub archetype: Option<String>,
    pub lists: usize,
    /// goldfish's names for these lists (the LDC's for lists from mtgo.com), most lists first
    pub goldfish: Vec<(String, usize)>,
    /// most main-deck copies first
    pub cards: Vec<Card>,
}

#[derive(Serialize, Deserialize)]
pub struct FrontRow {
    pub name: String,
    /// percent of goldfish's sample
    pub share: f64,
    pub decks: i32,
}

/// mtggoldfish's Legacy metagame page as of `fetched` (YYYY-MM-DD): `<data>/ldc/front.json`.
#[derive(Serialize, Deserialize)]
pub struct Front {
    pub fetched: String,
    pub rows: Vec<FrontRow>,
}

#[derive(Serialize)]
pub struct Meta {
    pub since: String,
    /// dates of the first and last event in the window
    pub first: Option<String>,
    pub last: Option<String>,
    pub events: usize,
    pub lists: usize,
    /// most lists first
    pub decks: Vec<Deck>,
    pub front: Option<Front>,
}

fn front_path(data: &Path) -> PathBuf {
    data.join("ldc").join("front.json")
}

fn parse_front(html: &str) -> Vec<FrontRow> {
    let name = regex::Regex::new(r##"<a href="/archetype/[^"#]+#online">([^<]*)</a>"##).unwrap();
    let stat = regex::Regex::new(r"archetype-tile-statistic-value'>\s*([\d.]+)%\s*<span[^>]*>\s*\((\d+)\)").unwrap();
    html.split("<div class='archetype-tile' id='")
        .skip(1)
        .filter_map(|tile| {
            let (n, s) = (name.captures(tile)?, stat.captures(tile)?);
            Some(FrontRow { name: ldc::unescape(n[1].trim()), share: s[1].parse().ok()?, decks: s[2].parse().ok()? })
        })
        .collect()
}

/// Downloads goldfish's Legacy metagame page to `<data>/ldc/front.json`; returns its deck count.
/// A page that parses to nothing leaves the stored copy alone.
pub fn fetch_front(data: &Path) -> Result<usize, String> {
    let html = String::from_utf8_lossy(&ldc::curl("https://www.mtggoldfish.com/metagame/legacy/full")?).to_string();
    let rows = parse_front(&html);
    if rows.is_empty() {
        return Err("mtggoldfish metagame page: no decks found".into());
    }
    let front = Front { fetched: chrono::Utc::now().date_naive().to_string(), rows };
    std::fs::create_dir_all(data.join("ldc")).map_err(|e| e.to_string())?;
    std::fs::write(front_path(data), serde_json::to_string(&front).unwrap()).map_err(|e| e.to_string())?;
    Ok(front.rows.len())
}

#[derive(QueryableByName)]
struct ListRow {
    #[diesel(sql_type = Text)]
    deck: String,
    #[diesel(sql_type = Nullable<Text>)]
    archetype: Option<String>,
    #[diesel(sql_type = Text)]
    goldfish_deck: String,
    #[diesel(sql_type = Text)]
    goldfish_id: String,
    #[diesel(sql_type = Text)]
    event: String,
    #[diesel(sql_type = Text)]
    date: String,
}

/// The decks of every published list of an event dated within `days` days of today.
pub fn meta(data: &Path, days: u32) -> Result<Meta, String> {
    let since = (chrono::Utc::now().date_naive() - chrono::Days::new(days as u64)).to_string();
    let conn = &mut ldc::DATASET.connect(data)?;
    let rows: Vec<ListRow> = diesel::sql_query(
        "SELECT coalesce(l.deck, nullif(l.goldfish_deck, ''), p.deck, '') AS deck, l.archetype, coalesce(nullif(l.goldfish_deck, ''), p.deck, '') AS goldfish_deck, l.goldfish_id, e.event, e.date
         FROM ldc_lists l JOIN ldc_events e ON e.event = l.event LEFT JOIN ldc_players p ON p.event = l.event AND p.player_key = l.player_key WHERE e.date >= ? ORDER BY e.date",
    )
    .bind::<Text, _>(&since)
    .load(conn)
    .map_err(|e| e.to_string())?;
    struct Acc {
        archetype: Option<String>,
        lists: usize,
        goldfish: BTreeMap<String, usize>,
        cards: BTreeMap<String, Card>,
    }
    let mut decks: BTreeMap<String, Acc> = BTreeMap::new();
    let mut events = std::collections::BTreeSet::new();
    let mut lists = 0;
    for r in &rows {
        let Ok(text) = std::fs::read_to_string(ldc::lists_dir(data).join(format!("{}.txt", r.goldfish_id))) else { continue };
        let Ok((main, side)) = reference::parse_decklist(&text) else { continue };
        events.insert(&r.event);
        lists += 1;
        let d = decks.entry(r.deck.clone()).or_insert_with(|| Acc { archetype: None, lists: 0, goldfish: BTreeMap::new(), cards: BTreeMap::new() });
        d.lists += 1;
        d.archetype = d.archetype.take().or_else(|| r.archetype.clone());
        *d.goldfish.entry(r.goldfish_deck.clone()).or_default() += 1;
        for (name, qty) in &main {
            let c = d.cards.entry(name.clone()).or_default();
            c.main_lists += 1;
            c.main_copies += qty;
        }
        for (name, qty) in &side {
            let c = d.cards.entry(name.clone()).or_default();
            c.side_lists += 1;
            c.side_copies += qty;
        }
    }
    let mut out: Vec<Deck> = decks
        .into_iter()
        .map(|(deck, a)| {
            let mut goldfish: Vec<(String, usize)> = a.goldfish.into_iter().collect();
            goldfish.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
            let mut cards: Vec<Card> = a.cards.into_iter().map(|(name, c)| Card { name, ..c }).collect();
            cards.sort_by(|x, y| y.main_copies.cmp(&x.main_copies).then(y.side_copies.cmp(&x.side_copies)).then(x.name.cmp(&y.name)));
            Deck { deck, archetype: a.archetype, lists: a.lists, goldfish, cards }
        })
        .collect();
    out.sort_by(|x, y| y.lists.cmp(&x.lists).then(x.deck.cmp(&y.deck)));
    let front = std::fs::read_to_string(front_path(data)).ok().and_then(|t| serde_json::from_str(&t).ok());
    Ok(Meta { since, first: rows.first().map(|r| r.date.clone()), last: rows.last().map(|r| r.date.clone()), events: events.len(), lists, decks: out, front })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_page_tiles() {
        let html = r##"<div class='archetype-tile-container'>
<div class='archetype-tile' id='1'><a href="/archetype/legacy-dimir-tempo#online">Dimir Tempo</a>
<div class='archetype-tile-statistic-value'>
7.1%
<span class='archetype-tile-statistic-value-extra-data'>
(174)
</span></div></div>
<div class='archetype-tile' id='2'><a href="/archetype/legacy-dnt#online">Death &amp; Taxes</a>
<div class='archetype-tile-statistic-value'>0.4%<span class='x'>(9)</span></div>
<div class='archetype-tile-statistic-value'>$ 5,216</div></div>"##;
        let rows = parse_front(html);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].name.as_str(), rows[0].share, rows[0].decks), ("Dimir Tempo", 7.1, 174));
        assert_eq!((rows[1].name.as_str(), rows[1].decks), ("Death & Taxes", 9));
    }
}
