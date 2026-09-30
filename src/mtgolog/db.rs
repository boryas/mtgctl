//! SQLite storage for parsed MTGO game logs (tables `mtgo_*` in mtgctl.db).
//!
//! Sides are normalised to "me"/"opp" at import so every analysis can slice on the same
//! columns without re-deriving who I was. The raw player names are kept on the match.

use super::event::Event;
use super::model::{CardEvent, GameOutcome, MatchLog, MatchResult, WinBy};
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
        era_id -> Nullable<Integer>,
        imported_at -> Text,
        my_list_id -> Nullable<Integer>,
        derived -> Integer,
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
        list_id -> Nullable<Integer>,
        my_dd_seq -> Nullable<Integer>,
        hands -> Nullable<Integer>,
        hand_known -> Bool,
        pile_known -> Bool,
    }
}

diesel::table! {
    mtgo_card_events (id) {
        id -> Integer,
        game_id -> Integer,
        match_id -> Integer,
        seq -> Integer,
        turn -> Integer,
        side -> Text,
        actor -> Text,
        card -> Text,
        catalog_id -> Integer,
        object_id -> Integer,
        verb -> Text,
        other -> Nullable<Text>,
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
        text -> Text,
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

diesel::allow_tables_to_appear_in_same_query!(mtgo_matches, mtgo_games, mtgo_card_events, mtgo_events, mtgo_eras);

const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS mtgo_matches (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        guid TEXT NOT NULL,              -- unique per me: both players of a match may upload it
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
        opp_hand INTEGER
    )",
    "CREATE TABLE IF NOT EXISTS mtgo_card_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        game_id INTEGER NOT NULL REFERENCES mtgo_games(id) ON DELETE CASCADE,
        match_id INTEGER NOT NULL,
        seq INTEGER NOT NULL,            -- mtgo_events.seq of the line
        turn INTEGER NOT NULL,
        side TEXT NOT NULL,              -- me | opp: whose card
        actor TEXT NOT NULL,             -- me | opp: whom the line is about
        card TEXT NOT NULL,
        catalog_id INTEGER NOT NULL,
        object_id INTEGER NOT NULL,      -- renumbered by MTGO on every zone change
        verb TEXT NOT NULL,              -- model::Verb::name
        other TEXT                       -- model::CardEvent::other
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
        text TEXT NOT NULL               -- raw line without the @P prefix
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
    "CREATE TABLE IF NOT EXISTS mtgo_state (
        key TEXT PRIMARY KEY,            -- e.g. explain_reviewed_through
        value TEXT NOT NULL
    )",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_mtgo_matches_me_guid ON mtgo_matches(me, guid)",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_games_match ON mtgo_games(match_id)",
    "DROP TABLE IF EXISTS mtgo_game_cards",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_card_events_game ON mtgo_card_events(game_id)",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_card_events_card ON mtgo_card_events(card, verb)",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_events_game ON mtgo_events(game_id)",
    "CREATE INDEX IF NOT EXISTS idx_mtgo_events_kind ON mtgo_events(kind)",
    "DROP INDEX IF EXISTS idx_mtgo_events_card",
];

#[derive(QueryableByName)]
pub struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub n: i64,
}

pub fn ensure_schema(conn: &mut SqliteConnection) -> QueryResult<()> {
    for stmt in DDL.iter().chain(super::decklist::DDL.iter()).chain([super::reference::ASSIGN_DDL, super::reference::MATCH_DECKS_DDL].iter()) {
        diesel::sql_query(*stmt).execute(conn)?;
    }
    // Columns added after the first release (ALTER if missing).
    for (table, col, ty) in [("mtgo_matches", "my_list_id", "INTEGER"), ("mtgo_games", "list_id", "INTEGER"), ("mtgo_games", "my_dd_seq", "INTEGER"), ("mtgo_games", "hands", "INTEGER"), ("mtgo_games", "hand_known", "BOOLEAN NOT NULL DEFAULT 0"), ("mtgo_games", "pile_known", "BOOLEAN NOT NULL DEFAULT 0"), ("mtgo_matches", "derived", "INTEGER NOT NULL DEFAULT 0"), ("mtgo_lists", "experimental", "BOOLEAN NOT NULL DEFAULT 0"), ("mtgo_lists", "fork", "TEXT"), ("mtgo_lists", "fork_experimental", "BOOLEAN NOT NULL DEFAULT 0"), ("mtgo_lists", "parent_set", "INTEGER"), ("mtgo_lists", "parent_id", "INTEGER"), ("mtgo_lists", "concept", "TEXT"), ("mtgo_decklists", "event_name", "TEXT"), ("mtgo_lists", "me", "TEXT"), ("mtgo_decklists", "me", "TEXT")] {
        if diesel::sql_query(format!("SELECT {col} FROM {table} LIMIT 0")).execute(conn).is_err() {
            diesel::sql_query(format!("ALTER TABLE {table} ADD COLUMN {col} {ty}")).execute(conn)?;
            if col == "me" {
                // a database from before the column held one player's rows
                diesel::sql_query(format!("UPDATE {table} SET me = (SELECT me FROM mtgo_matches GROUP BY me ORDER BY COUNT(*) DESC LIMIT 1)")).execute(conn)?;
            }
        }
    }
    // Columns replaced by mtgo_card_events. SQLite refuses to drop a column a view names, so the
    // view goes first; stats::ensure_view recreates it.
    for (table, col) in [("mtgo_events", "card"), ("mtgo_events", "catalog_id"), ("mtgo_events", "target"), ("mtgo_games", "my_lands"), ("mtgo_games", "opp_lands"), ("mtgo_games", "my_spells"), ("mtgo_games", "opp_spells")] {
        if diesel::sql_query(format!("SELECT {col} FROM {table} LIMIT 0")).execute(conn).is_ok() {
            diesel::sql_query("DROP VIEW IF EXISTS mtgo_game_view").execute(conn)?;
            diesel::sql_query(format!("ALTER TABLE {table} DROP COLUMN {col}")).execute(conn)?;
        }
    }
    for stmt in super::decklist::INDEXES {
        diesel::sql_query(*stmt).execute(conn)?;
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
}

#[derive(Insertable)]
#[diesel(table_name = mtgo_card_events)]
struct NewCardEvent<'a> {
    game_id: i32,
    match_id: i32,
    seq: i32,
    turn: i32,
    side: &'static str,
    actor: &'static str,
    card: &'a str,
    catalog_id: i32,
    object_id: i32,
    verb: &'static str,
    other: Option<&'a str>,
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
        PutsOnTop { .. } => "PutsOnTop",
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
    /// existing matches whose per-game log facts were rewritten, see DERIVED
    pub rederived: usize,
}

/// Version of the per-game facts `derive_games` writes (events, card events, Doomsday resolution);
/// bump it when they change and every sync rewrites the older matches from their logs.
const DERIVED: i32 = 1;

/// The player a one-player database belongs to: the configured username, else whoever most of its matches name as me.
pub fn player(conn: &mut SqliteConnection, cfg: &MtgoConfig) -> QueryResult<String> {
    if let Some(u) = &cfg.username {
        return Ok(u.clone());
    }
    let me: Option<String> = mtgo_matches::table.group_by(mtgo_matches::me).select(mtgo_matches::me).order(diesel::dsl::count_star().desc()).first(conn).optional()?;
    Ok(me.unwrap_or_default())
}

/// Delete every row of one player: their matches with everything hanging off them, and their lists.
pub fn remove_player(conn: &mut SqliteConnection, me: &str) -> QueryResult<usize> {
    let ids: Vec<i32> = mtgo_matches::table.filter(mtgo_matches::me.eq(me)).select(mtgo_matches::id).load(conn)?;
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        for id in &ids {
            delete_match(conn, *id)?;
        }
        super::decklist::delete_lists(conn, me)?;
        Ok(ids.len())
    })
}

/// Imports the logs under `root` as `cfg.username`'s (else the name most logs share); only that player's matches are compared or replaced.
pub fn import(conn: &mut SqliteConnection, cfg: &MtgoConfig, root: &Path, force: bool) -> QueryResult<ImportStats> {
    ensure_schema(conn)?;
    let (loaded, bad) = load_all(root);
    let me = cfg.username.clone().or_else(|| guess_me(&loaded)).unwrap_or_default();
    if force {
        let ids: Vec<i32> = mtgo_matches::table.filter(mtgo_matches::me.eq(&me)).select(mtgo_matches::id).load(conn)?;
        for id in ids {
            delete_match(conn, id)?;
        }
    }
    // guid → (id, imported_at, derived) of what is already in the DB
    let existing: HashMap<String, (i32, String, i32)> = mtgo_matches::table
        .filter(mtgo_matches::me.eq(&me))
        .select((mtgo_matches::guid, mtgo_matches::id, mtgo_matches::imported_at, mtgo_matches::derived))
        .load::<(String, i32, String, i32)>(conn)?
        .into_iter()
        .map(|(g, id, at, d)| (g, (id, at, d)))
        .collect();
    let now_dt = chrono::Local::now().naive_local();
    let now = now_dt.format("%Y-%m-%d %H:%M:%S").to_string();
    let mut st = ImportStats { matches: 0, skipped: 0, games: 0, cards: 0, events: 0, replays_skipped: 0, failed: bad.len(), in_progress: 0, refreshed: 0, rederived: 0 };

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
            if let Some((id, imported_at, derived)) = existing.get(&l.m.guid) {
                // Re-import if the file grew after we imported it (e.g. imported mid-match before the guard existed).
                let stale = match (mtime, chrono::NaiveDateTime::parse_from_str(imported_at, "%Y-%m-%d %H:%M:%S")) {
                    (Some(mt), Ok(at)) => mt > at,
                    _ => false,
                };
                if !stale {
                    if *derived < DERIVED {
                        derive_games(conn, *id, &l.m, &me, &mut st)?;
                        st.rederived += 1;
                    } else {
                        st.skipped += 1;
                    }
                    continue;
                }
                delete_match(conn, *id)?;
                st.refreshed += 1;
            }
            insert_match(conn, l, &me, &now, &mut st)?;
        }
        Ok(())
    })?;
    Ok(st)
}

/// Remove one match and everything hanging off it (foreign keys are not enforced by default in SQLite).
fn delete_match(conn: &mut SqliteConnection, id: i32) -> QueryResult<()> {
    for t in ["mtgo_events", "mtgo_card_events", "mtgo_games", "match_decks"] {
        diesel::sql_query(format!("DELETE FROM {t} WHERE match_id = {id}")).execute(conn)?;
    }
    diesel::sql_query(format!("DELETE FROM mtgo_matches WHERE id = {id}")).execute(conn)?;
    Ok(())
}

fn insert_match(conn: &mut SqliteConnection, l: &Loaded, me: &str, now: &str, st: &mut ImportStats) -> QueryResult<()> {
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
    let match_id: i32 = mtgo_matches::table.filter(mtgo_matches::me.eq(me)).filter(mtgo_matches::guid.eq(&m.guid)).select(mtgo_matches::id).first(conn)?;
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
            })
            .execute(conn)?;
        st.games += 1;
    }
    derive_games(conn, match_id, m, me, st)
}

/// (Re)writes the facts of a stored match's games that come from reading its log line by line:
/// its events, its card events and my Doomsday resolution; stamps the match with DERIVED.
fn derive_games(conn: &mut SqliteConnection, match_id: i32, m: &MatchLog, me: &str, st: &mut ImportStats) -> QueryResult<()> {
    for t in ["mtgo_events", "mtgo_card_events"] {
        diesel::sql_query(format!("DELETE FROM {t} WHERE game_id IN (SELECT id FROM mtgo_games WHERE match_id = {match_id})")).execute(conn)?;
    }
    let ids: HashMap<i32, i32> = mtgo_games::table.filter(mtgo_games::match_id.eq(match_id)).select((mtgo_games::game_number, mtgo_games::id)).load::<(i32, i32)>(conn)?.into_iter().collect();
    let opponent = m.opponent_of(me);
    for g in &m.games {
        let Some(&game_id) = ids.get(&(g.number as i32)) else { continue };
        let mut evs: Vec<NewEvent> = Vec::with_capacity(g.events.len());
        let mut turn = 0;
        for (i, le) in g.events.iter().enumerate() {
            if let Event::Turn { number, .. } = le.event {
                turn = number;
            }
            let actor = super::event::actor(&le.event).map(str::to_string);
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
                text: text.to_string(),
            });
        }
        st.events += evs.len();
        for chunk in evs.chunks(400) {
            diesel::insert_into(mtgo_events::table).values(chunk).execute(conn)?;
        }
        st.cards += insert_card_events(conn, game_id, match_id, me, opponent, &g.card_events)?;
        // the snapshot facts went with the card events; hands::derive_all writes them again
        diesel::update(mtgo_games::table.find(game_id)).set((mtgo_games::my_dd_seq.eq(g.doomsday_resolved.get(me).map(|&s| s as i32)), mtgo_games::hands.eq(None::<i32>))).execute(conn)?;
    }
    diesel::update(mtgo_matches::table.find(match_id)).set(mtgo_matches::derived.eq(DERIVED)).execute(conn)?;
    Ok(())
}

/// Inserts one game's card events; returns how many were kept.
pub(crate) fn insert_card_events(conn: &mut SqliteConnection, game_id: i32, match_id: i32, me: &str, opponent: Option<&str>, events: &[CardEvent]) -> QueryResult<usize> {
    // a log can name players besides me and the match's opponent (spectated games); their cards are left out
    let side = |p: &str| if p == me { Some("me") } else if Some(p) == opponent { Some("opp") } else { None };
    let rows: Vec<NewCardEvent> = events
        .iter()
        .filter_map(|e| {
            Some(NewCardEvent {
                game_id,
                match_id,
                seq: e.seq as i32,
                turn: e.turn as i32,
                side: side(&e.owner)?,
                actor: side(&e.actor)?,
                card: &e.card.name,
                catalog_id: e.card.catalog_id as i32,
                object_id: e.card.instance_id as i32,
                verb: e.verb.name(),
                other: e.other.as_deref(),
            })
        })
        .collect();
    for chunk in rows.chunks(500) {
        diesel::insert_into(mtgo_card_events::table).values(chunk).execute(conn)?;
    }
    Ok(rows.len())
}
