//! One player's upload directory → their rows of the pooled database: parse the game logs, link
//! the registered lists, and name both sides' decks against the reference decklists.
use super::MtgoConfig;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::path::{Path, PathBuf};

/// Layout under `<data>/players/<name>/`: the raw uploads, from which the player's rows of the
/// pooled database are rebuilt.
pub struct PlayerDir {
    pub root: PathBuf,
}

impl PlayerDir {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }
    /// The per-player database of builds before the pooled one; only `mtgctl-web rebuild` reads it.
    pub fn old_db(&self) -> PathBuf {
        self.root.join("mtgctl.db")
    }
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.logs())?;
        std::fs::create_dir_all(self.sessions())
    }
    /// A config pointing every path at this player, everything else default.
    pub fn config(&self, username: &str) -> MtgoConfig {
        MtgoConfig {
            log_dir: Some(self.logs().to_string_lossy().to_string()),
            username: Some(username.to_string()),
            session_logs_dir: Some(self.sessions().to_string_lossy().to_string()),
            ..MtgoConfig::default()
        }
    }
}

/// `<data>/mtgctl.db`, every player's rows (see `mtgo_matches.me`), in WAL mode so pages read
/// while a sync writes; `<data>/shared.db` is attached as `shared` when it exists (the reference
/// lists live there; a filter can ask whether the assigned reference runs a card).
pub fn connect(data: &Path) -> Result<SqliteConnection, String> {
    std::fs::create_dir_all(data).map_err(|e| e.to_string())?;
    let mut conn = crate::db::establish_connection_at(&data.join("mtgctl.db"))?;
    diesel::sql_query("PRAGMA journal_mode = WAL").execute(&mut conn).map_err(|e| e.to_string())?;
    let shared = data.join("shared.db");
    if shared.exists() {
        diesel::sql_query(format!("ATTACH DATABASE '{}' AS shared", shared.to_string_lossy().replace('\'', "''"))).execute(&mut conn).map_err(|e| e.to_string())?;
    }
    Ok(conn)
}

/// Player-independent inputs: catalog-id → card-name rows that Scryfall cannot answer (DFC
/// printings; only the MTGO client's own card database knows them), the reference decklists and
/// the Legacy timeline.
pub struct Reference {
    pub card_names: Option<PathBuf>,
    /// the shared database (reference decklists, card facts)
    pub shared_db: PathBuf,
    /// bans and set releases that open eras
    pub timeline: PathBuf,
}

impl Reference {
    /// `repo` holds `definitions/mtgo/legacy_timeline.toml` and `deploy/mtgo_cards_seed.sql`; `data` holds `shared.db`.
    pub fn locate(repo: &Path, data: &Path) -> Self {
        let seed = repo.join("deploy/mtgo_cards_seed.sql");
        Reference { card_names: seed.exists().then_some(seed), shared_db: data.join("shared.db"), timeline: repo.join("definitions/mtgo/legacy_timeline.toml") }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct SyncReport {
    pub imported: usize,
    pub refreshed: usize,
    pub skipped: usize,
    pub in_progress: usize,
    pub failed: usize,
    pub lists: usize,
    pub linked_games: usize,
    pub lists_assigned: usize,
    pub steps: Vec<String>,
}

/// Run the pipeline for one player: import → decklists → reference decks; only rows whose `me` is
/// `username` are rewritten. Each step's own console output goes to the server log.
pub fn sync(conn: &mut SqliteConnection, dir: &PlayerDir, username: &str, reference: &Reference) -> Result<SyncReport, String> {
    dir.ensure().map_err(|e| e.to_string())?;
    let cfg = dir.config(username);
    super::db::ensure_schema(conn).map_err(|e| e.to_string())?;
    let mut steps = Vec::new();
    let t0 = std::time::Instant::now();
    let mut lap = t0;
    let mut timed = |steps: &mut Vec<String>, msg: String| {
        let now = std::time::Instant::now();
        steps.push(format!("{msg} [{:.1}s]", (now - lap).as_secs_f64()));
        lap = now;
    };

    if let Some(seed) = &reference.card_names {
        for stmt in std::fs::read_to_string(seed).map_err(|e| e.to_string())?.lines().filter(|l| l.starts_with("INSERT")) {
            diesel::sql_query(stmt).execute(conn).map_err(|e| format!("card seed: {e}"))?;
        }
    }
    let st = super::db::import(conn, &cfg, &dir.logs(), false).map_err(|e| format!("import: {e}"))?;
    timed(&mut steps, format!("import: {} new, {} refreshed, {} re-derived, {} skipped, {} in progress, {} failed", st.matches, st.refreshed, st.rederived, st.skipped, st.in_progress, st.failed));
    let es = super::eras::stamp(conn, &reference.timeline, false).map_err(|e| format!("eras: {e}"))?;
    let stale = super::eras::staleness(conn, &reference.timeline, username)?.map(|(days, last)| format!("; the newest match is {days} days past the timeline's last event ({last}): definitions/mtgo/legacy_timeline.toml is probably missing a set release or B&R change")).unwrap_or_default();
    timed(&mut steps, format!("eras: {} matches placed in {} eras{}{stale}", es.placed, es.eras, es.from.map(|d| format!(", the timeline changed from {d} on")).unwrap_or_default()));

    // fetch = true: list catalog ids are printings the game logs never name; Scryfall fills them once per player DB.
    let dl = super::decklist::run_decklists(conn, &cfg, username, true, false).map_err(|e| format!("decklists: {e}"))?;
    timed(&mut steps, format!("decklists: {} lists, {} games linked, {} unlinked", dl.lists, dl.linked, dl.unlinked));
    let hs = super::hands::derive_all(conn, &cfg, &dir.logs(), username).map_err(|e| format!("hands: {e}"))?;
    timed(&mut steps, format!("hands: {} games read from session logs, {} with every hand fact known, {} Doomsday piles, {} without snapshots", hs.games, hs.hand_known, hs.pile_known, hs.no_snapshots));

    let shared = &mut crate::db::establish_connection_at(&reference.shared_db)?;
    let eras = super::eras::eras_from_file(&reference.timeline)?;
    let refs = super::reference::load_references(shared, &eras).map_err(|e| format!("references: {e}"))?;
    let assigned = super::reference::assign_player(conn, &refs, username).map_err(|e| format!("assign: {e}"))?;
    let lists_assigned = assigned.iter().filter(|a| a.deck.is_some()).count();
    timed(&mut steps, format!("decks: {lists_assigned} of {} lists named by {} reference decklists", assigned.len(), refs.refs.len()));
    let md = super::reference::label_matches(conn, &refs, username).map_err(|e| format!("match decks: {e}"))?;
    timed(&mut steps, format!("match decks: mine from list {}, from cards {} ({} placed on a registered list); opponents named {}, archetype only {}", md.me_list, md.me_cards, md.me_list_inferred, md.opp_named, md.opp_archetype_only));


    Ok(SyncReport { imported: st.matches, refreshed: st.refreshed, skipped: st.skipped, in_progress: st.in_progress, failed: st.failed, lists: dl.lists, linked_games: dl.linked, lists_assigned, steps })
}
