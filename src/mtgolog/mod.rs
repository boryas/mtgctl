//! MTGO client game-log analysis: parse the client's own `Match_GameLog_*.dat` files
//! (written under `AppData/Local/Apps/2.0/Data/.../AppFiles/`) into typed events and matches.
//!
//! See ~/org/projects/mtgctl/mtgo-logs.org for the format notes and roadmap.
#![allow(dead_code)] // parsed fields are consumed by upcoming analyses

pub mod cards;
pub mod cluster;
pub mod colors;
pub mod db;
pub mod decklist;
pub mod eras;
pub mod event;
pub mod format;
pub mod knn;
pub mod label;
pub mod ldc;
pub mod model;
pub mod picker;
pub mod pipeline;
pub mod reference;
pub mod pivot;
pub mod scan;
pub mod skeleton;
pub mod stats;

use clap::{Args, Subcommand};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Default location of the MTGO profile when MTGO runs in the docker/wine setup
/// (volume `mtgo-data-bo` mounted at `/home/wine/.wine/drive_c/users`).
pub const DEFAULT_LOG_DIR: &str = "/var/lib/docker/volumes/mtgo-data-bo/_data/wine/AppData/Local/Apps/2.0";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MtgoConfig {
    /// Root to search (recursively) for Match_GameLog_*.dat files.
    pub log_dir: Option<String>,
    /// Your MTGO username; auto-detected if unset.
    pub username: Option<String>,
    /// `mtgo stats -i`: columns pre-selected as filters.
    pub stats_filters: Vec<String>,
    /// `mtgo stats -i`: columns pre-selected as group-bys.
    pub stats_groupbys: Vec<String>,
    /// `mtgo stats -i`: histogram columns pre-selected.
    pub stats_hist: Vec<String>,
    /// `mtgo stats -i`: default minimum games per row.
    pub stats_min_games: Option<i64>,
    /// `mtgo stats -i`: row order pre-selected (games | wr | key | time).
    pub stats_sort: Option<String>,
    /// Directory of archived session logs (mtgo-YYYYmmdd-HHMMSS.log); default ~/.local/share/mtgo/logs.
    pub session_logs_dir: Option<String>,
    /// Marker rules file; default definitions/mtgo/markers.toml.
    pub markers: Option<String>,
    /// `mtgo stats -i`: curated first-screen columns for filters / group-bys (rest behind "more columns…").
    pub picker_filter_columns: Vec<String>,
    pub picker_groupby_columns: Vec<String>,
    /// Default scope (SQL over mtgo_game_view) applied to stats/cards/pivot/picker unless --all.
    /// Default: "has_decklist = 1" — only matches with an exact registered list.
    pub default_scope: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ConfigFile {
    mtgo: MtgoConfig,
}

pub fn load_config() -> MtgoConfig {
    std::fs::read_to_string("config.toml")
        .ok()
        .and_then(|s| toml::from_str::<ConfigFile>(&s).ok())
        .map(|c| c.mtgo)
        .unwrap_or_default()
}

impl MtgoConfig {
    pub fn log_dir(&self) -> PathBuf {
        PathBuf::from(self.log_dir.clone().unwrap_or_else(|| DEFAULT_LOG_DIR.to_string()))
    }
    pub fn session_logs_dir(&self) -> PathBuf {
        match &self.session_logs_dir {
            Some(d) => PathBuf::from(d),
            None => {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(format!("{home}/.local/share/mtgo/logs"))
            }
        }
    }
    /// Combine the default scope with a user WHERE (unless `all`). Returns (where, scope description).
    pub fn scope(&self, where_sql: Option<String>, all: bool) -> (Option<String>, String) {
        let scope = self.default_scope.clone().unwrap_or_else(|| "has_decklist = 1".to_string());
        if all || scope.trim().is_empty() {
            return (where_sql, "all matches".to_string());
        }
        let desc = format!("scope: {scope} (list-backed only; --all to include everything)");
        match where_sql {
            Some(w) if !w.trim().is_empty() => (Some(format!("({scope}) AND ({w})")), desc),
            _ => (Some(scope), desc),
        }
    }
    pub fn markers_path(&self) -> PathBuf {
        PathBuf::from(self.markers.clone().unwrap_or_else(|| "definitions/mtgo/markers.toml".to_string()))
    }
}

#[derive(Args)]
pub struct MtgoArgs {
    #[command(subcommand)]
    command: MtgoCommand,
}

#[derive(Subcommand)]
enum MtgoCommand {
    /// Parse all MTGO game logs and summarise (matches, games, results).
    Scan {
        /// Directory to search (default: [mtgo].log_dir in config.toml, else the docker volume).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// List every match.
        #[arg(short, long)]
        matches: bool,
        /// Only the last N matches.
        #[arg(long)]
        last: Option<usize>,
        /// Show the top N unparsed line templates (vocabulary census).
        #[arg(long, value_name = "N")]
        unparsed: Option<usize>,
        /// Dump all typed events for matches whose guid starts with this prefix.
        #[arg(long, value_name = "GUID")]
        dump: Option<String>,
        /// Include replay logs (re-watched games; duplicates of real matches).
        #[arg(long)]
        include_replays: bool,
    },
    /// Cluster card signatures (cards observed per match, mine and opponents') by cosine distance over TF-IDF vectors.
    /// These are NOT decklists: a signature is what was drawn/played; labels are best-effort archetype proxies.
    Cluster {
        /// Cosine-distance cut for average-linkage clustering (0 = identical, 1 = disjoint).
        #[arg(long, default_value_t = 0.65)]
        threshold: f32,
        /// Tighter cut applied inside each cluster to split it into variants (my_cards_variant / opp_cards_variant).
        #[arg(long, default_value_t = 0.4)]
        variant_threshold: f32,
        /// Use only cards that were cast/played (ignore revealed/discarded/milled evidence).
        #[arg(long)]
        played_only: bool,
        /// Variants: treat each of my matches separately instead of merging my cards per (day, cluster).
        #[arg(long)]
        me_per_match: bool,
        /// Print cluster counts for a range of thresholds instead of clustering.
        #[arg(long)]
        sweep: bool,
        /// Only print clusters with at least this many members.
        #[arg(long, default_value_t = 3)]
        min_show: usize,
        /// Cluster naming: cutoff (2–4 characteristic cards + an identifying land) or top2.
        #[arg(long, default_value = "cutoff")]
        naming: String,
        /// Print both naming schemes side by side for the shown clusters.
        #[arg(long)]
        compare_names: bool,
    },
    /// Group imported games by any column(s) of mtgo_game_view (see `mtgo columns`).
    Stats {
        /// Comma-separated columns to group by, e.g. opp_cards,on_play
        #[arg(long, value_delimiter = ',')]
        by: Vec<String>,
        /// One table per value of this column (e.g. --per my_deck --by opp_guess_archetype), instead of joint groups.
        #[arg(long, value_name = "COL")]
        per: Option<String>,
        /// SQL WHERE clause over mtgo_game_view, e.g. "my_cards LIKE 'Doomsday%' AND date >= '2026-01-01'"
        #[arg(long = "where")]
        where_sql: Option<String>,
        /// Hide groups with fewer matches (games, when a game-level column is involved).
        #[arg(long, default_value_t = 1)]
        min: i64,
        /// Row order: size (default: matches, or games in game mode; decks, eras, months and dates default to time), wr, key (the group value), or time (first appearance).
        #[arg(long, default_value = "games")]
        sort: String,
        #[arg(long, default_value_t = 60)]
        limit: usize,
        /// Game-level detail: game WR, turns, share of games, mulligans (all, win/loss), opp mulligans, turns win/loss.
        #[arg(long)]
        full: bool,
        /// Per-group top-5 histogram of a column (repeatable), e.g. --hist win_by --hist opp_cards
        #[arg(long, value_name = "COL")]
        hist: Vec<String>,
        /// Interactive: fuzzy-pick filters and group-bys (defaults from [mtgo] in config.toml).
        #[arg(short, long)]
        interactive: bool,
        /// Include every match (default scope is list-backed matches only: has_decklist = 1).
        #[arg(long)]
        all: bool,
    },
    /// Per-card win-rate lift: games where a card was played vs not (first-order, no confounder control).
    Cards {
        /// Whose cards: me or opp.
        #[arg(long, default_value = "me")]
        side: String,
        /// SQL WHERE over mtgo_game_view, e.g. "my_cards LIKE 'Doomsday%'" or "opp_cards LIKE 'Eldrazi%'"
        #[arg(long = "where")]
        where_sql: Option<String>,
        /// Only count the card if first cast/played on or before this turn (e.g. 1 for "T1 Chalice").
        #[arg(long)]
        by_turn: Option<i32>,
        /// Count cards merely seen (revealed/discarded/milled…) too, not just cast/played.
        #[arg(long)]
        seen: bool,
        /// Minimum games-with to list a card (default: no filtering).
        #[arg(long, default_value_t = 1)]
        min: i64,
        /// Order: lift (default), worst, n, name.
        #[arg(long, default_value = "lift")]
        sort: String,
        #[arg(long, default_value_t = 40)]
        limit: usize,
        /// Include every match (default scope is list-backed matches only).
        #[arg(long)]
        all: bool,
    },
    /// Label every match's deck (both sides) by a nearest-anchor vote over its whole card signature; anchors are matches whose deck is known (LDC sheets, hand entries, registered lists, manual labels).
    Label {
        /// Marker rules, only used with --rules / --suggest.
        #[arg(long, default_value = "definitions/mtgo/markers.toml")]
        file: PathBuf,
        /// LDC deck name → deck_types vocabulary.
        #[arg(long, default_value = "definitions/mtgo/ldc_decks.toml")]
        ldc_file: PathBuf,
        /// Neighbours voting per match.
        #[arg(long, default_value_t = 5)]
        k: usize,
        /// Minimum vote margin (best − second)/best to label; below it the match stays unknown.
        #[arg(long, default_value_t = 0.3)]
        margin: f32,
        /// Minimum cosine similarity to the nearest anchor; below it the signature resembles nothing known and stays unknown.
        #[arg(long, default_value_t = 0.35)]
        min_sim: f32,
        /// Use the old ordered single-marker rules instead of the anchor vote.
        #[arg(long)]
        rules: bool,
        /// Print, without writing, every opponent match since this date with its vote, nearest anchors and cards. Without a date: since the last review (stored when a review finishes).
        #[arg(long, value_name = "YYYY-MM-DD", num_args = 0..=1, default_missing_value = "last")]
        explain: Option<String>,
        /// With --explain: only matches that need a human — unknown, skeleton-labelled, or vote margin below 0.6.
        #[arg(long)]
        uncertain: bool,
        /// With --check: only anchors whose match is on or after this date (accuracy on what you labelled recently).
        #[arg(long, value_name = "YYYY-MM-DD")]
        since: Option<String>,
        /// Leave-one-out accuracy/coverage of the vote over the anchors (or, with --rules, the marker check vs hand entries).
        #[arg(long)]
        check: bool,
        /// Print a markers.toml skeleton for clusters (≥ N members) that no rule labels, then exit.
        #[arg(long, value_name = "N")]
        suggest: Option<usize>,
    },
    /// Pivot analysis for a staged plan: what happens before the pivot card resolves, after, and when it's never cast.
    Pivot {
        /// The pivot card (e.g. Doomsday, Marit Lage, Thassa's Oracle).
        #[arg(long, default_value = "Doomsday")]
        card: String,
        /// The conversion card cast after resolution (default: Thassa's Oracle when card = Doomsday).
        #[arg(long)]
        convert: Option<String>,
        /// SQL WHERE over mtgo_game_view to pick the population, e.g. "my_guess_archetype='Doomsday' AND my_guess_subtype='Tempo'"
        #[arg(long = "where")]
        where_sql: Option<String>,
        /// Minimum games-with for an event row (default: no filtering).
        #[arg(long, default_value_t = 1)]
        min: i64,
        /// Rows per direction in each table.
        #[arg(long, default_value_t = 12)]
        top: usize,
        /// Include every match (default scope is list-backed matches only).
        #[arg(long)]
        all: bool,
    },
    /// Import exact registered decklists from the client's session logs ("Deck Used in Game ID") and link them to games.
    Decklists {
        /// Resolve unknown catalog ids via Scryfall (curl); off = leave as mtgo#<id>.
        #[arg(long, default_value_t = true)]
        fetch: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Show the distinct registered lists (id, name, archetype, usage).
    Lists,
    /// Print one list (or the diff between two: `mtgo list 3 --diff 5`).
    List {
        id: i32,
        #[arg(long)]
        diff: Option<i32>,
    },
    /// Name a list (shows up as my_deck).
    NameList { id: i32, name: String },
    /// Tag lists as experimental brews (my_experimental = 1 in mtgo_game_view; the hard scope drops them).
    Experimental {
        ids: Vec<i32>,
        /// Clear the tag instead.
        #[arg(long)]
        off: bool,
    },
    /// Validate the labeller's uncertain opponent matches one at a time with the deck typeahead; Enter anchors the match, labels are rebuilt at the end.
    Review {
        /// Start date; default: where the last review stopped.
        #[arg(long, value_name = "YYYY-MM-DD")]
        since: Option<String>,
        #[arg(long, default_value = "definitions/mtgo/ldc_decks.toml")]
        ldc_file: PathBuf,
        #[arg(long, default_value = "definitions/mtgo/markers.toml")]
        file: PathBuf,
    },
    /// Import card colour facts (the card_colors.json written by ~/.local/share/mtgo/ldc/fetch_scryfall.py); `mtgo label` then fills my_colors/opp_colors.
    CardsImport { file: PathBuf },
    /// Import MTGGoldfish archetype skeletons (the skeletons.json written by ~/.local/share/mtgo/ldc/fetch_goldfish.py).
    SkeletonImport { file: PathBuf },
    /// Import LDC sheet data (the ldc_all.json written by ~/.local/share/mtgo/ldc/parse_ldc.py) into ldc_players.
    LdcImport { file: PathBuf },
    /// Hand-label one side of a match; overrides the guess in my_guess_*/opp_guess_* (e.g. `mtgo label-match 3428 opp Tempo --subtype Dimir`).
    LabelMatch {
        match_id: i32,
        /// me | opp
        side: String,
        archetype: String,
        #[arg(long)]
        subtype: Option<String>,
        /// Defaults to the archetype's category in markers.toml.
        #[arg(long)]
        category: Option<String>,
        #[arg(long, default_value = "definitions/mtgo/markers.toml")]
        file: PathBuf,
    },
    /// List the columns available to `mtgo stats`.
    Columns,
    /// Load the Legacy timeline (bans, set releases) into mtgo_eras and stamp matches with era_id.
    Eras {
        /// Timeline TOML (default: definitions/mtgo/legacy_timeline.toml).
        #[arg(long, default_value = "definitions/mtgo/legacy_timeline.toml")]
        file: PathBuf,
        /// Only banlist/manual events open eras (set releases are recorded but don't split).
        #[arg(long)]
        only_bans: bool,
        /// Verify banned cards vanish from the logs after their effective date.
        #[arg(long)]
        check: bool,
    },
    /// Rename a cluster (e.g. `mtgo name 3 "Dimir Tempo"`). Reset by re-running `mtgo cluster`.
    Name { id: i32, name: String },
    /// Import parsed game logs into mtgctl.db (tables mtgo_matches/games/game_cards/events). Idempotent.
    Import {
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Wipe the mtgo_* tables and re-import everything.
        #[arg(long)]
        force: bool,
        /// Skip the per-event table (much smaller DB, no replay/state queries).
        #[arg(long)]
        no_events: bool,
    },
}

pub fn run(args: MtgoArgs) {
    let cfg = load_config();
    match args.command {
        MtgoCommand::Scan { dir, matches, last, unparsed, dump, include_replays } => {
            let root: PathBuf = dir.unwrap_or_else(|| cfg.log_dir());
            scan::run_scan(&cfg, Path::new(&root), matches, unparsed, dump.as_deref(), last, include_replays);
        }
        MtgoCommand::Cluster { threshold, variant_threshold, played_only, me_per_match, sweep, min_show, naming, compare_names } => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = cluster::run_cluster(conn, threshold, variant_threshold, played_only, me_per_match, sweep, min_show, &naming, compare_names) {
                eprintln!("cluster failed: {e}");
            }
        }
        MtgoCommand::Stats { by, per, where_sql, min, sort, limit, full, hist, interactive, all } => {
            let conn = &mut crate::db::establish_connection();
            let (where_sql, scope_desc) = cfg.scope(where_sql, all);
            let mut q = stats::StatsQuery { by, per, where_sql, min_games: min, sort, limit, full, hist };
            println!("{scope_desc}");
            if interactive {
                match picker::pick(conn, &cfg, q) {
                    Ok(Some(picked)) => q = picked,
                    Ok(None) => return,
                    Err(e) => {
                        eprintln!("{e}");
                        return;
                    }
                }
                println!("\n$ {}\n", q.command_line());
            }
            if let Err(e) = stats::run_stats(conn, &q) {
                eprintln!("stats failed: {e}");
            }
        }
        MtgoCommand::Eras { file, only_bans, check } => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = eras::run_eras(conn, &file, only_bans, check) {
                eprintln!("eras failed: {e}");
            }
        }
        MtgoCommand::Cards { side, where_sql, by_turn, seen, min, sort, limit, all } => {
            let conn = &mut crate::db::establish_connection();
            let (where_sql, scope_desc) = cfg.scope(where_sql, all);
            println!("{scope_desc}");
            if let Err(e) = cards::run_cards(conn, &side, where_sql.as_deref(), by_turn, seen, min, &sort, limit) {
                eprintln!("cards failed: {e}");
            }
        }
        MtgoCommand::Label { file, ldc_file, k, margin, min_sim, rules, check, suggest, explain, uncertain, since } => {
            let conn = &mut crate::db::establish_connection();
            let r = if rules || suggest.is_some() { label::run_label(conn, &file, check, suggest).map_err(|e| e.to_string()) } else { knn::run(conn, &ldc_file, &file, cfg.username.as_deref().unwrap_or(""), k, margin, min_sim, check, since.as_deref(), explain.as_deref(), uncertain) };
            if let Err(e) = r {
                eprintln!("label failed: {e}");
            }
        }
        MtgoCommand::Review { since, ldc_file, file } => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = knn::review(conn, &ldc_file, &file, cfg.username.as_deref().unwrap_or(""), since.as_deref()) {
                eprintln!("review failed: {e}");
            }
        }
        MtgoCommand::CardsImport { file } => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = colors::run_import(conn, &file) {
                eprintln!("cards-import failed: {e}");
            }
        }
        MtgoCommand::SkeletonImport { file } => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = skeleton::run_import(conn, &file) {
                eprintln!("skeleton-import failed: {e}");
            }
        }
        MtgoCommand::LdcImport { file } => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = ldc::run_import(conn, &file) {
                eprintln!("ldc-import failed: {e}");
            }
        }
        MtgoCommand::Pivot { card, convert, where_sql, min, top, all } => {
            let conn = &mut crate::db::establish_connection();
            let (wher, scope_desc) = cfg.scope(where_sql, all);
            println!("{scope_desc}");
            if let Err(e) = pivot::run_pivot(conn, &card, convert.as_deref(), wher.as_deref(), min, top) {
                eprintln!("pivot failed: {e}");
            }
        }
        MtgoCommand::Decklists { fetch, verbose } => {
            let conn = &mut crate::db::establish_connection();
            match decklist::run_decklists(conn, &cfg, fetch, verbose) {
                Ok(st) => println!(
                    "decklists: {} 'Deck Used' games, {} new distinct lists, {} linked to imported games, {} unlinked (game not imported yet), {} mismatched, {} unresolved card ids",
                    st.lines, st.lists, st.linked, st.unlinked, st.mismatched, st.unknown_names
                ),
                Err(e) => eprintln!("decklists failed: {e}"),
            }
        }
        MtgoCommand::Lists => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = decklist::print_lists(conn) {
                eprintln!("{e}");
            }
        }
        MtgoCommand::List { id, diff } => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = decklist::print_list(conn, id, diff) {
                eprintln!("{e}");
            }
        }
        MtgoCommand::NameList { id, name } => {
            use diesel::prelude::*;
            let conn = &mut crate::db::establish_connection();
            match diesel::sql_query(format!("UPDATE mtgo_lists SET name = '{}' WHERE id = {id}", name.replace('\'', "''"))).execute(conn) {
                Ok(1) => println!("list {id} → {name}"),
                Ok(_) => eprintln!("no list {id}"),
                Err(e) => eprintln!("{e}"),
            }
        }
        MtgoCommand::Experimental { ids, off } => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = db::ensure_schema(conn) {
                eprintln!("{e}");
                return;
            }
            for id in ids {
                match decklist::set_experimental(conn, id, !off) {
                    Ok(()) => println!("list {id}: experimental = {}", !off),
                    Err(e) => eprintln!("{e}"),
                }
            }
        }
        MtgoCommand::LabelMatch { match_id, side, archetype, subtype, category, file } => {
            let conn = &mut crate::db::establish_connection();
            let rules = label::load_markers(&file).unwrap_or_default();
            match label::label_match(conn, &rules, match_id, &side, &archetype, subtype.as_deref(), category.as_deref()) {
                Ok(()) => println!("match {match_id} {side} → {archetype}{} (source = manual)", subtype.map(|s| format!(" / {}", knn::canon_subtype(&s))).unwrap_or_default()),
                Err(e) => eprintln!("{e}"),
            }
        }
        MtgoCommand::Columns => {
            let conn = &mut crate::db::establish_connection();
            if let Err(e) = stats::print_columns(conn) {
                eprintln!("{e}");
            }
        }
        MtgoCommand::Name { id, name } => {
            use diesel::prelude::*;
            let conn = &mut crate::db::establish_connection();
            match diesel::update(db::mtgo_clusters::table.filter(db::mtgo_clusters::id.eq(id))).set(db::mtgo_clusters::name.eq(&name)).execute(conn) {
                Ok(1) => println!("cluster {id} → {name}"),
                Ok(_) => eprintln!("no cluster {id}"),
                Err(e) => eprintln!("{e}"),
            }
        }
        MtgoCommand::Import { dir, force, no_events } => {
            let root: PathBuf = dir.unwrap_or_else(|| cfg.log_dir());
            let conn = &mut crate::db::establish_connection();
            match db::import(conn, &cfg, &root, force, !no_events) {
                Ok(st) => println!(
                    "imported {} matches ({} refreshed, {} already present, {} in progress, {} replays skipped, {} files failed): {} games, {} game-card rows, {} events",
                    st.matches, st.refreshed, st.skipped, st.in_progress, st.replays_skipped, st.failed, st.games, st.cards, st.events
                ),
                Err(e) => eprintln!("import failed: {e}"),
            }
        }
    }
}
