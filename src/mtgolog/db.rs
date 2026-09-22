//! SQLite storage for parsed MTGO game logs (tables `mtgo_*` in mtgctl.db).
//!
//! Sides are normalised to "me"/"opp" at import so every analysis can slice on the same
//! columns without re-deriving who I was. The raw player names are kept on the match.

use super::event::{Event, Target};
use super::model::{GameOutcome, MatchLog, MatchResult, WinBy};
use super::scan::{guess_me, load_all, Loaded};
use super::MtgoConfig;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;
use std::path::Path;

diesel::table! {
    mtgo_matches (id) {
        id -> Integer,
        guid -> Text,
        path -> Text,
        start_ts -> Nullable<Text>,
        end_ts -> Nullable<Text>,
        me -> Text,
        opponent -> Nullable<Text>,
        result -> Text,
        my_game_wins -> Integer,
        opp_game_wins -> Integer,
        games -> Integer,
        replay -> Bool,
        record_count -> Integer,
        event_name -> Nullable<Text>,
        format -> Nullable<Text>,
        my_cluster -> Nullable<Integer>,
        opp_cluster -> Nullable<Integer>,
        era_id -> Nullable<Integer>,
        imported_at -> Text,
        my_variant -> Nullable<Integer>,
        opp_variant -> Nullable<Integer>,
        my_list_id -> Nullable<Integer>,
    }
}

diesel::table! {
    mtgo_variants (id) {
        id -> Integer,
        cluster_id -> Integer,
        size -> Integer,
        name -> Text,
        top_cards -> Text,
        threshold -> Double,
    }
}

diesel::table! {
    mtgo_games (id) {
        id -> Integer,
        match_id -> Integer,
        game_number -> Integer,
        start_ts -> Text,
        end_ts -> Text,
        duration_secs -> Integer,
        on_play -> Nullable<Bool>,
        turns -> Integer,
        winner -> Nullable<Text>,
        win_by -> Nullable<Text>,
        my_mulligans -> Integer,
        opp_mulligans -> Integer,
        my_hand -> Nullable<Integer>,
        opp_hand -> Nullable<Integer>,
        my_lands -> Integer,
        opp_lands -> Integer,
        my_spells -> Integer,
        opp_spells -> Integer,
        list_id -> Nullable<Integer>,
    }
}

diesel::table! {
    mtgo_game_cards (id) {
        id -> Integer,
        game_id -> Integer,
        match_id -> Integer,
        side -> Text,
        name -> Text,
        catalog_id -> Integer,
        instances -> Integer,
        plays -> Integer,
        first_turn -> Nullable<Integer>,
        played -> Bool,
    }
}

diesel::table! {
    mtgo_events (id) {
        id -> Integer,
        game_id -> Integer,
        match_id -> Integer,
        seq -> Integer,
        ts -> Text,
        turn -> Integer,
        actor -> Nullable<Text>,
        side -> Nullable<Text>,
        kind -> Text,
        card -> Nullable<Text>,
        catalog_id -> Nullable<Integer>,
        target -> Nullable<Text>,
        text -> Text,
    }
}

diesel::table! {
    mtgo_clusters (id) {
        id -> Integer,
        size -> Integer,
        name -> Nullable<Text>,
        top_cards -> Text,
        threshold -> Double,
        created_at -> Text,
    }
}

diesel::table! {
    mtgo_eras (id) {
        id -> Integer,
        start_date -> Text,
        name -> Text,
        kind -> Text,
        banned -> Text,
        unbanned -> Text,
        notes -> Text,
        source -> Text,
    }
}

diesel::table! {
    mtgo_match_labels (match_id, side, source) {
        match_id -> Integer,
        side -> Text,
        source -> Text,
        archetype -> Nullable<Text>,
        subtype -> Nullable<Text>,
        category -> Nullable<Text>,
        archetype_marker -> Nullable<Text>,
        subtype_marker -> Nullable<Text>,
    }
}

diesel::allow_tables_to_appear_in_same_query!(mtgo_matches, mtgo_games, mtgo_game_cards, mtgo_events, mtgo_clusters, mtgo_eras, mtgo_variants, mtgo_match_labels);

const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS mtgo_matches (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        guid TEXT NOT NULL UNIQUE,
        path TEXT NOT NULL,
        start_ts TEXT,
        end_ts TEXT,
        me TEXT NOT NULL,
        opponent TEXT,
        result TEXT NOT NULL,            -- W | L | T | ?
        my_game_wins INTEGER NOT NULL,
        opp_game_wins INTEGER NOT NULL,
        games INTEGER NOT NULL,
        replay BOOLEAN NOT NULL DEFAULT 0,
        record_count INTEGER NOT NULL,
        event_name TEXT,                 -- from mtgo_game_history (later)
        format TEXT,
        my_cluster INTEGER,              -- mtgo_clusters.id (deck signature cluster)
        opp_cluster INTEGER,
        era_id INTEGER,
        imported_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_games (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        match_id INTEGER NOT NULL REFERENCES mtgo_matches(id) ON DELETE CASCADE,
        game_number INTEGER NOT NULL,
        start_ts TEXT NOT NULL,
        end_ts TEXT NOT NULL,
        duration_secs INTEGER NOT NULL,
        on_play BOOLEAN,                 -- me on the play
        turns INTEGER NOT NULL,
        winner TEXT,                     -- me | opp | NULL (unfinished)
        win_by TEXT,                     -- concession | normal | opponent_left | loss_line
        my_mulligans INTEGER NOT NULL,
        opp_mulligans INTEGER NOT NULL,
        my_hand INTEGER,
        opp_hand INTEGER,
        my_lands INTEGER NOT NULL,
        opp_lands INTEGER NOT NULL,
        my_spells INTEGER NOT NULL,
        opp_spells INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_game_cards (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        game_id INTEGER NOT NULL REFERENCES mtgo_games(id) ON DELETE CASCADE,
        match_id INTEGER NOT NULL,
        side TEXT NOT NULL,              -- me | opp
        name TEXT NOT NULL,
        catalog_id INTEGER NOT NULL,
        instances INTEGER NOT NULL,      -- distinct object ids seen (~copies)
        plays INTEGER NOT NULL,          -- casts + land drops
        first_turn INTEGER,              -- first turn cast/played
        played BOOLEAN NOT NULL          -- 1 = cast/played; 0 = only seen (revealed/discarded/milled/...)
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        game_id INTEGER NOT NULL REFERENCES mtgo_games(id) ON DELETE CASCADE,
        match_id INTEGER NOT NULL,
        seq INTEGER NOT NULL,
        ts TEXT NOT NULL,
        turn INTEGER NOT NULL,
        actor TEXT,
        side TEXT,                       -- me | opp | NULL
        kind TEXT NOT NULL,              -- Event variant name
        card TEXT,                       -- primary card name (cast/played/source)
        catalog_id INTEGER,
        target TEXT,                     -- first target (card name or player side)
        text TEXT NOT NULL               -- raw line without the @P prefix
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_clusters (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        size INTEGER NOT NULL,
        name TEXT,
        top_cards TEXT NOT NULL,
        threshold DOUBLE NOT NULL,
        created_at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_eras (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        start_date TEXT NOT NULL,        -- effective date (MTGO) of the change that opens the era
        name TEXT NOT NULL,
        kind TEXT NOT NULL,              -- banlist | set | manual
        banned TEXT NOT NULL DEFAULT '', -- comma-separated
        unbanned TEXT NOT NULL DEFAULT '',
        notes TEXT NOT NULL DEFAULT '',
        source TEXT NOT NULL DEFAULT ''
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_variants (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        cluster_id INTEGER NOT NULL,     -- parent mtgo_clusters.id
        size INTEGER NOT NULL,
        name TEXT NOT NULL,              -- cards distinguishing this variant from its siblings
        top_cards TEXT NOT NULL,
        threshold DOUBLE NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_match_labels (
        match_id INTEGER NOT NULL,
        side TEXT NOT NULL,              -- me | opp
        source TEXT NOT NULL,            -- guess (marker rules) | list (Deck Used logs) | manual
        archetype TEXT,
        subtype TEXT,
        category TEXT,
        archetype_marker TEXT,           -- the card that triggered the archetype guess
        subtype_marker TEXT,
        PRIMARY KEY (match_id, side, source)
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_state (
        key TEXT PRIMARY KEY,            -- e.g. explain_reviewed_through
        value TEXT NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_games_match ON mtgo_games(match_id)",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_game_cards_game ON mtgo_game_cards(game_id)",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_game_cards_name ON mtgo_game_cards(name)",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_events_game ON mtgo_events(game_id)",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_events_kind ON mtgo_events(kind)",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_events_card ON mtgo_events(card)",
];

#[derive(QueryableByName)]
pub struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub n: i64,
}

pub fn ensure_schema(conn: &mut SqliteConnection) -> QueryResult<()> {
    for stmt in DDL.iter().chain(super::decklist::DDL.iter()).chain(super::ldc::DDL.iter()).chain(super::skeleton::DDL.iter()).chain(super::colors::DDL.iter()) {
        diesel::sql_query(*stmt).execute(conn)?;
    }
    // Columns added after the first release (ALTER if missing).
    for (table, col, ty) in [("mtgo_matches", "my_variant", "INTEGER"), ("mtgo_matches", "opp_variant", "INTEGER"), ("mtgo_matches", "my_list_id", "INTEGER"), ("mtgo_games", "list_id", "INTEGER"), ("mtgo_lists", "experimental", "BOOLEAN NOT NULL DEFAULT 0"), ("mtgo_lists", "fork", "TEXT"), ("mtgo_lists", "fork_experimental", "BOOLEAN NOT NULL DEFAULT 0"), ("mtgo_lists", "parent_set", "INTEGER"), ("mtgo_lists", "parent_id", "INTEGER"), ("mtgo_lists", "concept", "TEXT"), ("mtgo_decklists", "event_name", "TEXT")] {
        if diesel::sql_query(format!("SELECT {col} FROM {table} LIMIT 0")).execute(conn).is_err() {
            diesel::sql_query(format!("ALTER TABLE {table} ADD COLUMN {col} {ty}")).execute(conn)?;
        }
    }
    Ok(())
}

/// DELETE everything from a table and reset its AUTOINCREMENT counter.
pub fn truncate(conn: &mut SqliteConnection, table: &str) -> QueryResult<()> {
    diesel::sql_query(format!("DELETE FROM {table}")).execute(conn)?;
    diesel::sql_query(format!("DELETE FROM sqlite_sequence WHERE name = '{table}'")).execute(conn)?;
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = mtgo_matches)]
struct NewMatch<'a> {
    guid: &'a str,
    path: String,
    start_ts: Option<String>,
    end_ts: Option<String>,
    me: &'a str,
    opponent: Option<&'a str>,
    result: &'a str,
    my_game_wins: i32,
    opp_game_wins: i32,
    games: i32,
    replay: bool,
    record_count: i32,
    imported_at: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = mtgo_games)]
struct NewGame {
    match_id: i32,
    game_number: i32,
    start_ts: String,
    end_ts: String,
    duration_secs: i32,
    on_play: Option<bool>,
    turns: i32,
    winner: Option<&'static str>,
    win_by: Option<&'static str>,
    my_mulligans: i32,
    opp_mulligans: i32,
    my_hand: Option<i32>,
    opp_hand: Option<i32>,
    my_lands: i32,
    opp_lands: i32,
    my_spells: i32,
    opp_spells: i32,
}

#[derive(Insertable)]
#[diesel(table_name = mtgo_game_cards)]
struct NewGameCard {
    game_id: i32,
    match_id: i32,
    side: &'static str,
    name: String,
    catalog_id: i32,
    instances: i32,
    plays: i32,
    first_turn: Option<i32>,
    played: bool,
}

#[derive(Insertable)]
#[diesel(table_name = mtgo_events)]
struct NewEvent {
    game_id: i32,
    match_id: i32,
    seq: i32,
    ts: String,
    turn: i32,
    actor: Option<String>,
    side: Option<&'static str>,
    kind: &'static str,
    card: Option<String>,
    catalog_id: Option<i32>,
    target: Option<String>,
    text: String,
}

pub fn kind_name(e: &Event) -> &'static str {
    use Event::*;
    match e {
        Rolled { .. } => "Rolled",
        Joined { .. } => "Joined",
        PlaysFirst { .. } => "PlaysFirst",
        DrawsFirst { .. } => "DrawsFirst",
        Mulligan { .. } => "Mulligan",
        BeginsGame { .. } => "BeginsGame",
        Turn { .. } => "Turn",
        SkipsDraw { .. } => "SkipsDraw",
        Draws { .. } => "Draws",
        Casts { .. } => "Casts",
        PlaysLand { .. } => "PlaysLand",
        Activates { .. } => "Activates",
        Triggers { .. } => "Triggers",
        Attacks { .. } => "Attacks",
        Blocks { .. } => "Blocks",
        Discards { .. } => "Discards",
        Mills { .. } => "Mills",
        Exiles { .. } => "Exiles",
        Sacrifices { .. } => "Sacrifices",
        ToGraveyard { .. } => "ToGraveyard",
        Reveals { .. } => "Reveals",
        Returns { .. } => "Returns",
        Cycles { .. } => "Cycles",
        Counters { .. } => "Counters",
        Shuffles { .. } => "Shuffles",
        GainsLife { .. } => "GainsLife",
        DamagePrevented { .. } => "DamagePrevented",
        Concedes { .. } => "Concedes",
        LeavesGame { .. } => "LeavesGame",
        WinsGame { .. } => "WinsGame",
        LosesGame { .. } => "LosesGame",
        LeadsMatch { .. } => "LeadsMatch",
        WinsMatch { .. } => "WinsMatch",
        MatchTied { .. } => "MatchTied",
        System(_) => "System",
        Other(_) => "Other",
    }
}

/// Primary card of an event (what was cast/played/activated/etc.).
fn primary_card(e: &Event) -> Option<(String, u32)> {
    use Event::*;
    let c = match e {
        Casts { card, .. } | PlaysLand { card, .. } | Cycles { card, .. } => card,
        Activates { source, .. } => source,
        Triggers { source: Some(source), .. } => source,
        Counters { spell, .. } => spell,
        Blocks { blocker, .. } => blocker,
        Draws { via: Some(c), .. } => c,
        Attacks { attackers, .. } => attackers.first()?,
        Discards { cards, .. } | Mills { cards, .. } | Exiles { cards, .. } | Sacrifices { cards, .. } | ToGraveyard { cards, .. } | Reveals { cards, .. } | Returns { cards, .. } => cards.first()?,
        DamagePrevented { source: Some(c), .. } => c,
        _ => return None,
    };
    Some((c.name.clone(), c.catalog_id))
}

fn first_target(e: &Event, me: &str) -> Option<String> {
    use Event::*;
    let ts = match e {
        Casts { targets, .. } | Activates { targets, .. } | Triggers { targets, .. } => targets,
        Blocks { attacker, .. } => return Some(attacker.name.clone()),
        Counters { with, .. } => return Some(with.clone()),
        _ => return None,
    };
    ts.first().map(|t| match t {
        Target::Card(c) => c.name.clone(),
        Target::Player(p) => side_of(p, me).unwrap_or("?").to_string(),
    })
}

fn side_of(player: &str, me: &str) -> Option<&'static str> {
    if player.is_empty() {
        None
    } else if player == me {
        Some("me")
    } else {
        Some("opp")
    }
}

fn fmt_ts(t: chrono::NaiveDateTime) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub struct ImportStats {
    pub matches: usize,
    pub skipped: usize,
    pub games: usize,
    pub cards: usize,
    pub events: usize,
    pub replays_skipped: usize,
    pub failed: usize,
    /// files modified in the last 10 minutes (match may still be running) — left for next time
    pub in_progress: usize,
    /// existing matches whose file grew after import → deleted and re-imported (counted in `matches` too)
    pub refreshed: usize,
}

pub fn import(conn: &mut SqliteConnection, cfg: &MtgoConfig, root: &Path, force: bool, with_events: bool) -> QueryResult<ImportStats> {
    ensure_schema(conn)?;
    let (loaded, bad) = load_all(root);
    let me = cfg.username.clone().or_else(|| guess_me(&loaded)).unwrap_or_default();
    if force {
        for t in ["mtgo_events", "mtgo_game_cards", "mtgo_games", "mtgo_matches"] {
            diesel::sql_query(format!("DELETE FROM {t}")).execute(conn)?;
        }
    }
    // guid → (id, imported_at) of what is already in the DB
    let existing: HashMap<String, (i32, String)> = mtgo_matches::table
        .select((mtgo_matches::guid, mtgo_matches::id, mtgo_matches::imported_at))
        .load::<(String, i32, String)>(conn)?
        .into_iter()
        .map(|(g, id, at)| (g, (id, at)))
        .collect();
    let now_dt = chrono::Local::now().naive_local();
    let now = now_dt.format("%Y-%m-%d %H:%M:%S").to_string();
    let mut st = ImportStats { matches: 0, skipped: 0, games: 0, cards: 0, events: 0, replays_skipped: 0, failed: bad.len(), in_progress: 0, refreshed: 0 };

    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        for l in &loaded {
            if l.m.replay {
                st.replays_skipped += 1;
                continue;
            }
            // A log with a final "wins the match" / "Match Tied" line is complete whatever its age.
            // One without it is still being played — unless it has sat untouched for 10 minutes,
            // in which case it is a match that never got a result line (disconnect, client crash)
            // and is imported as unknown.
            let mtime = std::fs::metadata(&l.path).and_then(|m| m.modified()).ok().map(chrono::DateTime::<chrono::Local>::from).map(|t| t.naive_local());
            let finished = !matches!(l.m.result, MatchResult::Unknown);
            if !finished && mtime.is_some_and(|mt| (now_dt - mt).num_minutes() < 10) {
                st.in_progress += 1;
                continue;
            }
            if let Some((id, imported_at)) = existing.get(&l.m.guid) {
                // Re-import if the file grew after we imported it (e.g. imported mid-match before the guard existed).
                let stale = match (mtime, chrono::NaiveDateTime::parse_from_str(imported_at, "%Y-%m-%d %H:%M:%S")) {
                    (Some(mt), Ok(at)) => mt > at,
                    _ => false,
                };
                if !stale {
                    st.skipped += 1;
                    continue;
                }
                delete_match(conn, *id)?;
                st.refreshed += 1;
            }
            insert_match(conn, l, &me, &now, with_events, &mut st)?;
        }
        Ok(())
    })?;
    Ok(st)
}

/// Remove one match and everything hanging off it (foreign keys are not enforced by default in SQLite).
fn delete_match(conn: &mut SqliteConnection, id: i32) -> QueryResult<()> {
    for t in ["mtgo_events", "mtgo_game_cards", "mtgo_games"] {
        diesel::sql_query(format!("DELETE FROM {t} WHERE match_id = {id}")).execute(conn)?;
    }
    diesel::sql_query(format!("DELETE FROM mtgo_match_labels WHERE match_id = {id}")).execute(conn)?;
    diesel::sql_query(format!("DELETE FROM mtgo_matches WHERE id = {id}")).execute(conn)?;
    Ok(())
}

fn insert_match(conn: &mut SqliteConnection, l: &Loaded, me: &str, now: &str, with_events: bool, st: &mut ImportStats) -> QueryResult<()> {
    let m: &MatchLog = &l.m;
    let opponent = m.opponent_of(me);
    let (w, lo) = m.game_score(me);
    let result = match &m.result {
        MatchResult::Won { winner, .. } if winner == me => "W",
        MatchResult::Won { .. } => "L",
        MatchResult::Tied { .. } => "T",
        MatchResult::Unknown => "?",
    };
    diesel::insert_into(mtgo_matches::table)
        .values(NewMatch {
            guid: &m.guid,
            path: l.path.display().to_string(),
            start_ts: m.start.map(fmt_ts),
            end_ts: m.end.map(fmt_ts),
            me,
            opponent,
            result,
            my_game_wins: w as i32,
            opp_game_wins: lo as i32,
            games: m.games.len() as i32,
            replay: m.replay,
            record_count: m.record_count as i32,
            imported_at: now,
        })
        .execute(conn)?;
    let match_id: i32 = mtgo_matches::table.filter(mtgo_matches::guid.eq(&m.guid)).select(mtgo_matches::id).first(conn)?;
    st.matches += 1;

    for g in &m.games {
        let mine = g.per_player.get(me);
        let theirs = opponent.and_then(|o| g.per_player.get(o));
        let (winner, win_by) = match &g.outcome {
            GameOutcome::Won { winner, by } => (
                Some(if winner == me { "me" } else { "opp" }),
                Some(match by {
                    WinBy::Concession => "concession",
                    WinBy::OpponentLeft(_) => "opponent_left",
                    WinBy::LossLine => "loss_line",
                    WinBy::Normal => "normal",
                }),
            ),
            GameOutcome::Unfinished => (None, None),
        };
        diesel::insert_into(mtgo_games::table)
            .values(NewGame {
                match_id,
                game_number: g.number as i32,
                start_ts: fmt_ts(g.start),
                end_ts: fmt_ts(g.end),
                duration_secs: g.duration_secs() as i32,
                on_play: g.first_player.as_deref().map(|p| p == me),
                turns: g.turns as i32,
                winner,
                win_by,
                my_mulligans: mine.map_or(0, |p| p.mulligans) as i32,
                opp_mulligans: theirs.map_or(0, |p| p.mulligans) as i32,
                my_hand: mine.and_then(|p| p.hand_size).map(|h| h as i32),
                opp_hand: theirs.and_then(|p| p.hand_size).map(|h| h as i32),
                my_lands: mine.map_or(0, |p| p.lands_played) as i32,
                opp_lands: theirs.map_or(0, |p| p.lands_played) as i32,
                my_spells: mine.map_or(0, |p| p.spells_cast) as i32,
                opp_spells: theirs.map_or(0, |p| p.spells_cast) as i32,
            })
            .execute(conn)?;
        let game_id: i32 = mtgo_games::table.order(mtgo_games::id.desc()).select(mtgo_games::id).first(conn)?;
        st.games += 1;

        let mut cards: Vec<NewGameCard> = Vec::new();
        for (side, pg) in [("me", mine), ("opp", theirs)] {
            let Some(pg) = pg else { continue };
            for (name, use_) in &pg.seen {
                let played = pg.played.get(name);
                cards.push(NewGameCard {
                    game_id,
                    match_id,
                    side,
                    name: name.clone(),
                    catalog_id: use_.catalog_id as i32,
                    instances: use_.instances.len() as i32,
                    plays: played.map_or(0, |p| p.plays) as i32,
                    first_turn: played.and_then(|p| p.first_turn).map(|t| t as i32),
                    played: played.is_some(),
                });
            }
        }
        st.cards += cards.len();
        for chunk in cards.chunks(500) {
            diesel::insert_into(mtgo_game_cards::table).values(chunk).execute(conn)?;
        }

        if with_events {
            let mut evs: Vec<NewEvent> = Vec::with_capacity(g.events.len());
            let mut turn = 0;
            for (i, le) in g.events.iter().enumerate() {
                if let Event::Turn { number, .. } = le.event {
                    turn = number;
                }
                let actor = super::event::actor(&le.event).map(str::to_string);
                let (card, catalog_id) = primary_card(&le.event).map(|(n, c)| (Some(n), Some(c as i32))).unwrap_or((None, None));
                let (text, _) = super::event::strip_prefix(&le.raw);
                evs.push(NewEvent {
                    game_id,
                    match_id,
                    seq: i as i32,
                    ts: fmt_ts(le.ts),
                    turn: turn as i32,
                    side: actor.as_deref().and_then(|a| side_of(a, me)),
                    actor,
                    kind: kind_name(&le.event),
                    card,
                    catalog_id,
                    target: first_target(&le.event, me),
                    text: text.to_string(),
                });
            }
            st.events += evs.len();
            for chunk in evs.chunks(400) {
                diesel::insert_into(mtgo_events::table).values(chunk).execute(conn)?;
            }
        }
    }
    Ok(())
}
