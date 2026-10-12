//! A dataset the stats engine slices: one flat view with a row per game in its own database,
//! and the catalog of its columns the stats page offers. The engine (`stats::compute`) and the
//! page read only the catalog, so a new dataset needs a view and a catalog, not new UI.

use diesel::sqlite::SqliteConnection;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Match,
    Game,
}

/// How the page edits a filter on the column: `num` compares unquoted (an expression without type
/// affinity never equals a quoted literal), `colors` is a WUBRG set, `date` a range.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Num,
    Colors,
    Date,
}

#[derive(Serialize)]
pub struct Column {
    pub name: &'static str,
    pub label: &'static str,
    pub level: Level,
    pub kind: Kind,
    /// display text for raw values, e.g. on_play 1 → "on the play"
    pub values: &'static [(&'static str, &'static str)],
}

/// What the page may offer beyond filtering and grouping on columns.
#[derive(Serialize)]
pub struct Features {
    /// card sentences over `mtgo_card_events` and decklist filters
    pub cards: bool,
    /// side panels for registered lists and reference decklists, and game logs and replays
    pub lists: bool,
    /// the rows view (one row per match or game)
    pub rows: bool,
    /// rows belong to uploading players; the page defaults to its own player
    pub players: bool,
    /// per-game turns, minutes and mulligans (the averages among the output statistics)
    pub game_details: bool,
}

#[derive(Serialize)]
pub struct Dataset {
    pub key: &'static str,
    pub title: &'static str,
    pub about: &'static str,
    #[serde(skip)]
    pub view: &'static str,
    pub columns: &'static [Column],
    /// Every per-game column of the view, offered or not: a query naming one is per game.
    #[serde(skip)]
    pub game_cols: &'static [&'static str],
    pub features: Features,
    /// Filters every default query starts with (a fresh page, a `#col=value` link), in the page's
    /// filter-entry shape; `start_filters` are added only on a fresh page.
    #[serde(skip)]
    pub default_filters: &'static str,
    #[serde(skip)]
    pub start_filters: &'static str,
    pub default_group: &'static str,
    #[serde(skip)]
    pub(crate) connect: fn(&Path) -> Result<SqliteConnection, String>,
    #[serde(skip)]
    pub(crate) ensure: fn(&mut SqliteConnection) -> Result<(), String>,
}

impl Dataset {
    /// A connection for reading; the view is assumed in place (see `open`).
    pub fn connect(&self, data: &Path) -> Result<SqliteConnection, String> {
        (self.connect)(data)
    }
    /// A connection with the tables and view in place (writes when they are not).
    pub fn open(&self, data: &Path) -> Result<SqliteConnection, String> {
        let mut conn = (self.connect)(data)?;
        (self.ensure)(&mut conn)?;
        Ok(conn)
    }
    /// `{defaults, start}`: see `default_filters`.
    pub fn defaults(&self) -> serde_json::Value {
        let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s).expect("filters are JSON");
        serde_json::json!({ "defaults": parse(self.default_filters), "start": parse(self.start_filters) })
    }
    pub fn is_game_col(&self, col: &str) -> bool {
        self.game_cols.contains(&col)
    }
}

pub const ALL: &[&Dataset] = &[&MTGO, &super::ldc::DATASET];

pub fn by_key(key: &str) -> Option<&'static Dataset> {
    ALL.iter().copied().find(|d| d.key == key)
}

const fn col(name: &'static str, label: &'static str, level: Level, kind: Kind) -> Column {
    Column { name, label, level, kind, values: &[] }
}
const fn colv(name: &'static str, label: &'static str, level: Level, kind: Kind, values: &'static [(&'static str, &'static str)]) -> Column {
    Column { name, label, level, kind, values }
}
use Kind::*;
use Level::*;
const YES_NO: &[(&str, &str)] = &[("0", "no"), ("1", "yes")];

/// The uploaded MTGO logs. Columns left out of the catalog (ids, raw outcomes, legacy label
/// columns) are deliberately unavailable on the page; see the audit in mtgo-web.org.
pub static MTGO: Dataset = Dataset {
    key: "mtgo",
    title: "MTGO logs",
    about: "Games from uploaded MTGO logs, with the cards played.",
    view: "mtgo_game_view",
    columns: &[
        col("my_deck", "my deck", Match, Text),
        col("my_archetype", "my archetype", Match, Text),
        colv("my_deck_grade", "my deck list source", Match, Text, &[("hard", "exact from logs"), ("inferred", "inferred from game"), ("none", "unknown")]),
        col("my_list", "my list (version)", Match, Text),
        colv("my_experimental", "experimental list", Match, Num, YES_NO),
        col("opp_deck", "opponent deck", Match, Text),
        col("opp_archetype", "opponent archetype", Match, Text),
        col("opp_deck_candidates", "opponent deck (candidates)", Match, Text),
        col("opp_colors", "opponent colours", Match, Colors),
        col("my_colors", "my colours", Match, Colors),
        col("player", "player", Match, Text),
        col("opponent", "opponent", Match, Text),
        col("match_result", "match result", Match, Text),
        col("event_type", "event type", Match, Text),
        col("event_name", "event", Match, Text),
        col("week", "week", Match, Text),
        col("month", "month", Match, Text),
        col("date", "date", Match, Date),
        col("era", "era", Match, Text),
        col("game_number", "game number", Game, Num),
        colv("on_play", "on the play", Game, Text, &[("0", "on the draw"), ("1", "on the play")]),
        col("my_mulligans", "my mulligans", Game, Num),
        col("opp_mulligans", "opp mulligans", Game, Num),
        col("my_hand", "my opening hand size", Game, Num),
        col("opp_hand", "opp opening hand size", Game, Num),
        col("win_by", "how the game ended", Game, Text),
        col("turns", "turns", Game, Num),
        col("minutes", "minutes", Game, Num),
        col("my_lands", "lands I played", Game, Num),
        col("opp_lands", "lands opp played", Game, Num),
        col("my_spells", "spells I cast", Game, Num),
        col("opp_spells", "spells opp cast", Game, Num),
        colv("my_dd_resolved", "my Doomsday resolved", Game, Num, YES_NO),
        col("my_dd_turn", "turn my Doomsday resolved", Game, Num),
        colv("my_hand_known", "my hands known (session log)", Game, Num, YES_NO),
        colv("my_pile_known", "my Doomsday pile known", Game, Num, YES_NO),
    ],
    game_cols: &[
        "game_id", "game_number", "on_play", "turns", "duration_secs", "minutes", "winner", "won", "win_by",
        "my_mulligans", "opp_mulligans", "my_hand", "opp_hand", "my_lands", "opp_lands", "my_spells", "opp_spells",
        "my_dd_resolved", "my_dd_turn", "my_hand_known", "my_pile_known",
    ],
    features: Features { cards: true, lists: true, rows: true, players: true, game_details: true },
    // exact lists only, no brews; a fresh page also leaves out opponents nothing explains
    default_filters: r#"[{"field":"my_deck_grade","op":"is","not":false,"values":["hard"]},
                         {"field":"my_experimental","op":"is","not":false,"values":["0"]}]"#,
    start_filters: r#"[{"field":"opp_archetype","op":"is","not":true,"values":[null]}]"#,
    default_group: "opp_archetype",
    connect: super::pipeline::connect,
    ensure: |c| {
        super::db::ensure_schema(c).map_err(|e| e.to_string())?;
        super::stats::ensure_view(c).map_err(|e| e.to_string())
    },
};
