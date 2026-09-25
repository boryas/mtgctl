//! MTGO client game-log analysis: parse the client's own `Match_GameLog_*.dat` files
//! (written under `AppData/Local/Apps/2.0/Data/.../AppFiles/`) into typed events and matches.
//!
//! See ~/org/projects/mtgctl/mtgo-logs.org for the format notes and roadmap.
#![allow(dead_code)] // parsed fields are consumed by upcoming analyses

pub mod cards;
pub mod db;
pub mod decklist;
pub mod eras;
pub mod event;
pub mod format;
pub mod model;
pub mod picker;
pub mod pipeline;
pub mod reference;
pub mod pivot;
pub mod scan;
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
    /// `mtgo stats -i`: curated first-screen columns for filters / group-bys (rest behind "more columns…").
    pub picker_filter_columns: Vec<String>,
    pub picker_groupby_columns: Vec<String>,
    /// Default scope (SQL over mtgo_game_view) applied to stats/cards/pivot/picker unless --all.
    /// Default: "has_decklist = 1" — only matches with an exact registered list.
    pub default_scope: Option<String>,
    /// The stats site `mtgo stats` reads from without --data; default https://bur.io/doomsday/stats.
    pub site: Option<String>,
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
    pub fn site(&self) -> String {
        self.site.clone().unwrap_or_else(|| "https://bur.io/doomsday/stats".to_string())
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
    /// Group imported games by any column(s) of mtgo_game_view (see `mtgo columns`).
    Stats {
        /// Comma-separated columns to group by, e.g. opp_archetype,on_play
        #[arg(long, value_delimiter = ',')]
        by: Vec<String>,
        /// One table per value of this column (e.g. --per my_deck --by opp_archetype), instead of joint groups.
        #[arg(long, value_name = "COL")]
        per: Option<String>,
        /// SQL WHERE clause over mtgo_game_view, e.g. "my_deck LIKE '%DD' AND date >= '2026-01-01'"
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
        /// Per-group top-5 histogram of a column (repeatable), e.g. --hist win_by --hist opp_deck
        #[arg(long, value_name = "COL")]
        hist: Vec<String>,
        /// Interactive: fuzzy-pick filters and group-bys (defaults from [mtgo] in config.toml).
        #[arg(short, long)]
        interactive: bool,
        /// Include every match (default scope is list-backed matches only: has_decklist = 1).
        #[arg(long)]
        all: bool,
        /// Whose matches (default: [mtgo] username); '*' for every player.
        #[arg(long)]
        player: Option<String>,
        /// Read the pooled database that `mtgctl-web --data <DIR> rebuild` made instead of the stats site.
        #[arg(long, value_name = "DIR")]
        data: Option<PathBuf>,
    },
    /// Per-card win-rate lift: games where a card was played vs not (first-order, no confounder control).
    Cards {
        /// Whose cards: me or opp.
        #[arg(long, default_value = "me")]
        side: String,
        /// SQL WHERE over mtgo_game_view, e.g. "my_archetype = 'Doomsday'" or "opp_archetype = 'Eldrazi'"
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
    /// Pivot analysis for a staged plan: what happens before the pivot card resolves, after, and when it's never cast.
    Pivot {
        /// The pivot card (e.g. Doomsday, Marit Lage, Thassa's Oracle).
        #[arg(long, default_value = "Doomsday")]
        card: String,
        /// The conversion card cast after resolution (default: Thassa's Oracle when card = Doomsday).
        #[arg(long)]
        convert: Option<String>,
        /// SQL WHERE over mtgo_game_view to pick the population, e.g. "my_deck = 'UB Tempo DD'"
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
    /// Show the distinct registered lists (id, name, archetype, usage).
    Lists,
    /// Print one list (or the diff between two: `mtgo list 3 --diff 5`).
    List {
        id: i32,
        #[arg(long)]
        diff: Option<i32>,
    },
    /// List the columns available to `mtgo stats`.
    Columns {
        /// As for `mtgo stats --data`.
        #[arg(long, value_name = "DIR")]
        data: Option<PathBuf>,
    },
}

pub fn run(args: MtgoArgs) {
    let cfg = load_config();
    match args.command {
        MtgoCommand::Scan { dir, matches, last, unparsed, dump, include_replays } => {
            let root: PathBuf = dir.unwrap_or_else(|| cfg.log_dir());
            scan::run_scan(&cfg, Path::new(&root), matches, unparsed, dump.as_deref(), last, include_replays);
        }
        MtgoCommand::Stats { by, per, where_sql, min, sort, limit, full, hist, interactive, all, player, data } => {
            let mut src = match stats::Source::open(data.as_deref(), cfg.site()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{e}");
                    return;
                }
            };
            let player = player.or_else(|| cfg.username.clone()).filter(|p| p != "*");
            let (where_sql, scope_desc) = cfg.scope(where_sql, all);
            let where_sql = match (&player, where_sql) {
                (Some(p), Some(w)) => Some(format!("player = '{}' AND ({w})", p.replace('\'', "''"))),
                (Some(p), None) => Some(format!("player = '{}'", p.replace('\'', "''"))),
                (None, w) => w,
            };
            let mut q = stats::StatsQuery { by, per, where_sql, min_games: min, sort, limit, full, hist };
            println!("{} · {scope_desc} · {}", player.as_deref().unwrap_or("every player"), data.as_ref().map_or_else(|| cfg.site(), |d| d.display().to_string()));
            if interactive {
                match picker::pick(&mut src, &cfg, q, player.as_deref()) {
                    Ok(Some(picked)) => q = picked,
                    Ok(None) => return,
                    Err(e) => {
                        eprintln!("{e}");
                        return;
                    }
                }
                println!("\n$ {}\n", q.command_line());
            }
            if let Err(e) = stats::run_stats(&mut src, &q) {
                eprintln!("stats failed: {e}");
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
        MtgoCommand::Pivot { card, convert, where_sql, min, top, all } => {
            let conn = &mut crate::db::establish_connection();
            let (wher, scope_desc) = cfg.scope(where_sql, all);
            println!("{scope_desc}");
            if let Err(e) = pivot::run_pivot(conn, &card, convert.as_deref(), wher.as_deref(), min, top) {
                eprintln!("pivot failed: {e}");
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
        MtgoCommand::Columns { data } => {
            let mut src = match stats::Source::open(data.as_deref(), cfg.site()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{e}");
                    return;
                }
            };
            if let Err(e) = stats::print_columns(&mut src) {
                eprintln!("{e}");
            }
        }
    }
}
