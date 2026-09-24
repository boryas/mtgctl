//! One player's upload directory → their database. Deliberately minimal: parse the game logs,
//! link the registered lists, cluster the card signatures. Opponent identification is a cluster
//! name a human gives; no marker rules, reference dumps or anchor votes are involved.
use super::MtgoConfig;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::path::{Path, PathBuf};

/// Layout under `<data>/players/<name>/`.
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
    pub fn db(&self) -> PathBuf {
        self.root.join("mtgctl.db")
    }
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.logs())?;
        std::fs::create_dir_all(self.sessions())
    }
    /// The player's database, with the shared database attached as `shared` when it exists (the
    /// reference lists live there; a filter can ask whether the assigned reference runs a card).
    pub fn connect(&self) -> Result<SqliteConnection, String> {
        let mut conn = crate::db::establish_connection_at(&self.db())?;
        if let Some(shared) = self.root.parent().and_then(|p| p.parent()).map(|d| d.join("shared.db")).filter(|p| p.exists()) {
            use diesel::prelude::*;
            diesel::sql_query(format!("ATTACH DATABASE '{}' AS shared", shared.to_string_lossy().replace('\'', "''"))).execute(&mut conn).map_err(|e| e.to_string())?;
        }
        Ok(conn)
    }
    /// A config pointing every path at this player, everything else default.
    pub fn config(&self, username: &str, reference: &Reference) -> MtgoConfig {
        MtgoConfig {
            log_dir: Some(self.logs().to_string_lossy().to_string()),
            username: Some(username.to_string()),
            session_logs_dir: Some(self.sessions().to_string_lossy().to_string()),
            markers: Some(reference.markers.to_string_lossy().to_string()),
            ..MtgoConfig::default()
        }
    }
}

/// Player-independent inputs: the marker rules (only to give my own lists a readable lineage
/// name, see decklist::auto_name_lists) and catalog-id → card-name rows that Scryfall cannot
/// answer (DFC printings; only the MTGO client's own card database knows them).
pub struct Reference {
    pub markers: PathBuf,
    pub card_names: Option<PathBuf>,
    /// the shared database (reference decklists, card facts)
    pub shared_db: PathBuf,
    /// bans and set releases that open eras
    pub timeline: PathBuf,
}

impl Reference {
    /// `repo` holds `definitions/mtgo/markers.toml` and `deploy/mtgo_cards_seed.sql`; `data` holds `shared.db`.
    pub fn locate(repo: &Path, data: &Path) -> Self {
        let seed = repo.join("deploy/mtgo_cards_seed.sql");
        Reference { markers: repo.join("definitions/mtgo/markers.toml"), card_names: seed.exists().then_some(seed), shared_db: data.join("shared.db"), timeline: repo.join("definitions/mtgo/legacy_timeline.toml") }
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

/// Run the pipeline for one player: import → decklists → cluster. Each step's own console
/// output goes to the server log.
pub fn sync(dir: &PlayerDir, username: &str, reference: &Reference) -> Result<SyncReport, String> {
    dir.ensure().map_err(|e| e.to_string())?;
    let cfg = dir.config(username, reference);
    let conn = &mut dir.connect()?;
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
    let st = super::db::import(conn, &cfg, &dir.logs(), false, true).map_err(|e| format!("import: {e}"))?;
    timed(&mut steps, format!("import: {} new, {} refreshed, {} skipped, {} in progress, {} failed", st.matches, st.refreshed, st.skipped, st.in_progress, st.failed));

    // fetch = true: list catalog ids are printings the game logs never name; Scryfall fills them once per player DB.
    let dl = super::decklist::run_decklists(conn, &cfg, true, false).map_err(|e| format!("decklists: {e}"))?;
    timed(&mut steps, format!("decklists: {} lists, {} games linked, {} unlinked", dl.lists, dl.linked, dl.unlinked));

    let shared = &mut crate::db::establish_connection_at(&reference.shared_db)?;
    let eras = super::eras::eras_from_file(&reference.timeline)?;
    let refs = super::reference::load_references(shared, &eras).map_err(|e| format!("references: {e}"))?;
    let assigned = super::reference::assign_player(conn, &refs).map_err(|e| format!("assign: {e}"))?;
    let lists_assigned = assigned.iter().filter(|a| a.deck.is_some()).count();
    timed(&mut steps, format!("decks: {lists_assigned} of {} lists named by {} reference decklists", assigned.len(), refs.refs.len()));
    let md = super::reference::label_matches(conn, &refs).map_err(|e| format!("match decks: {e}"))?;
    timed(&mut steps, format!("match decks: mine from list {}, from cards {} ({} placed on a registered list); opponents named {}, archetype only {}", md.me_list, md.me_cards, md.me_list_inferred, md.opp_named, md.opp_archetype_only));

    super::cluster::run_cluster(conn, 0.65, 0.4, false, false, false, usize::MAX, "cutoff", false).map_err(|e| format!("cluster: {e}"))?;
    timed(&mut steps, "cluster: ok".into());

    // Guessed opponent labels are not part of this pipeline; clear any left by an older build.
    diesel::sql_query("DELETE FROM mtgo_match_labels WHERE source = 'guess'").execute(conn).map_err(|e| e.to_string())?;

    Ok(SyncReport { imported: st.matches, refreshed: st.refreshed, skipped: st.skipped, in_progress: st.in_progress, failed: st.failed, lists: dl.lists, linked_games: dl.linked, lists_assigned, steps })
}
