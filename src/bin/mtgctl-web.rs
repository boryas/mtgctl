//! HTTP front-end over the mtgolog pipeline: players upload their MTGO log files, the server
//! pools every player's matches in one database and answers the same stats queries as `mtgctl mtgo stats`.
//! Identity (v0): the MTGO username the game logs themselves name as "me" — detected by
//! `/api/detect`, sent as the `X-Player` header, and re-checked against the stored logs on every
//! sync. No secrets; a real login (Discord) is the planned replacement.
//! Layout: `<data>/players/<name>/{logs,sessions}` (the uploads), `<data>/mtgctl.db` (derived from
//! them, see pipeline::connect), `<data>/shared.db` (reference decklists).

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use mtgctl::mtgolog::pipeline::{self, PlayerDir, Reference};
use mtgctl::mtgolog::{picker, stats};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tokio::sync::Mutex;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

#[derive(Parser)]
#[command(name = "mtgctl-web")]
struct Cli {
    /// Data root: players.toml plus one directory per player.
    #[arg(long, default_value = "web-data")]
    data: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Replace raw session logs stored by older builds with gzipped extracts.
    CompactSessions,
    /// Add a reference decklist to the shared database from a decklist file (`4 Brainstorm` lines, sideboard after a blank line or a `Sideboard` heading).
    RefAdd {
        #[arg(long)]
        archetype: String,
        #[arg(long)]
        deck: String,
        /// How the list is known, e.g. "Legacy Challenge 32 2026-09-06 5th, boryas".
        #[arg(long)]
        name: String,
        /// YYYY-MM-DD the list was played; its era (bans/sets) is resolved from the timeline.
        #[arg(long)]
        date: String,
        /// URL or note.
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        by: Option<String>,
        file: PathBuf,
    },
    /// List the reference decklists.
    RefList,
    /// Remove a reference decklist by id.
    RefRemove { id: i32 },
    /// Re-resolve every reference's era from its list date against the timeline.
    RefReera,
    /// Move a deck under another archetype, e.g. `ref-move Elves "GSZ/Cradle"`.
    RefMove { deck: String, archetype: String },
    /// Fold a deck into another, e.g. `ref-merge "Jeskai Tempo" "Azorius Tempo"`: its references become versions of the other.
    RefMerge { deck: String, into: String },
    /// Delete one stored session extract of a player (by file name); the next upload of that log stores it again.
    SessionRemove { player: String, name: String },
    /// Rebuild the pooled database from every player's uploads, carrying list names and experimental
    /// tags (by list hash) over from the per-player databases of older builds.
    Rebuild {
        /// Repo root holding definitions/mtgo/legacy_timeline.toml.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Delete a player: their rows of the pooled database and their uploads.
    PlayerRemove { player: String },
    /// Assign a player's registered lists to decks via the nearest reference decklist and print the decisions.
    Assign { player: String },
    /// Load card facts (card_colors.json from fetch_scryfall.py: colours, colours lands produce) into the shared database.
    CardsImport { file: PathBuf },
    /// Infer my deck per match from the cards I played; score it against the matches whose list is known, then summarise the rest.
    Infer {
        player: String,
        /// me (default) or opp
        #[arg(long, default_value = "me")]
        side: String,
        /// Pool the cards seen over all matches of a day before inferring (my side plays one list per session).
        #[arg(long)]
        pool_day: bool,
        /// Pool over runs of consecutive matches in a day while the pooled cards still fit one reference (a list switch breaks the run).
        #[arg(long)]
        pool_run: bool,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1:8090")]
        listen: String,
        /// Static files (the stats and collector pages).
        #[arg(long, default_value = "web")]
        web: PathBuf,
        /// Repo root holding definitions/mtgo/legacy_timeline.toml.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
}


struct AppState {
    data: PathBuf,
    reference: Reference,
    /// one pipeline run at a time: SQLite has one writer
    sync_lock: Mutex<()>,
}

/// Uploads per player are capped so a stray or hostile client cannot fill the volume.
const MAX_PLAYER_BYTES: u64 = 300 * 1024 * 1024;

type Shared = Arc<AppState>;

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn internal(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
}

/// The player named by the `X-Player` header or `?player=`: an MTGO username as the logs spell it.
fn auth(_state: &AppState, headers: &HeaderMap, q: &HashMap<String, String>) -> Result<String, ApiError> {
    let name = headers
        .get("x-player")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| q.get("player").cloned())
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "missing player".into()))?;
    valid_player(&name).then_some(name).ok_or(ApiError(StatusCode::BAD_REQUEST, "bad player name".into()))
}

/// MTGO usernames: letters, digits, `_ . -`; also keeps the name safe as a directory component.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn valid_player(n: &str) -> bool {
    !n.is_empty() && n.len() <= 32 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-') && !n.starts_with('.')
}

/// The username present in (nearly) every uploaded match: the same rule the CLI import uses.
fn detect_me(logs: &[Vec<u8>]) -> Option<(String, usize, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut parsed = 0usize;
    for b in logs {
        let Ok(raw) = mtgctl::mtgolog::format::parse_bytes(b) else { continue };
        parsed += 1;
        for p in mtgctl::mtgolog::model::build_match(&raw).players {
            *counts.entry(p).or_default() += 1;
        }
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(p, c)| (p, c, parsed))
}

/// Eras from the repo timeline (the CLI runs from the repo root or ~/mtgctl-web, both hold definitions/).
fn timeline_eras() -> Vec<(String, String)> {
    mtgctl::mtgolog::eras::eras_from_file(Path::new("definitions/mtgo/legacy_timeline.toml")).unwrap_or_else(|e| { eprintln!("timeline: {e}"); std::process::exit(1) })
}

/// The Windows scripts are meant to be read in the browser before they are run: serve them as text.
async fn plain_text_scripts(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let (ps1, cmd) = (req.uri().path().ends_with(".ps1"), req.uri().path().ends_with(".cmd"));
    let mut res = next.run(req).await;
    if res.status().is_success() {
        if ps1 {
            res.headers_mut().insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("text/plain; charset=utf-8"));
        } else if cmd {
            // the double-click installer: always a download (its text is shown on the collector page)
            res.headers_mut().insert(axum::http::header::CONTENT_DISPOSITION, axum::http::HeaderValue::from_static("attachment; filename=\"install-mtgctl.cmd\""));
        }
    }
    res
}

/// The shared database: reference decklists (and, later, everything that is not one player's).
fn shared_db(data: &Path) -> Result<diesel::SqliteConnection, String> {
    std::fs::create_dir_all(data).map_err(|e| e.to_string())?;
    let conn = mtgctl::db::establish_connection_at(&data.join("shared.db"))?;
    Ok(conn)
}

fn pool(state: &AppState) -> Result<diesel::SqliteConnection, String> {
    pipeline::connect(&state.data)
}

fn player_dir(state: &AppState, name: &str) -> PlayerDir {
    player_dir_at(&state.data, name)
}

fn player_dir_at(data: &Path, name: &str) -> PlayerDir {
    PlayerDir::new(data.join("players").join(name))
}

#[derive(Serialize)]
struct StoredFile {
    kind: String,
    name: String,
    size: u64,
    mtime: u64,
    sha256: String,
}

fn stored_files(dir: &PlayerDir) -> Vec<StoredFile> {
    let mut out = Vec::new();
    for (kind, d) in [("game", dir.logs()), ("session", dir.sessions())] {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if !md.is_file() {
                continue;
            }
            let mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
            let name = e.file_name().to_string_lossy().to_string();
            // session extracts carry the hash of the raw upload in their name (the collector dedups on that)
            let sha256 = session_hash_in_name(&name).unwrap_or_else(|| std::fs::read(e.path()).map(|b| hex(&Sha256::digest(&b))).unwrap_or_default());
            out.push(StoredFile { kind: kind.into(), name, size: md.len(), mtime, sha256 });
        }
    }
    out
}

/// `mtgo-YYYYmmdd-HHMMSS-<sha256>.log.gz` → the sha256.
fn session_hash_in_name(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".log.gz")?;
    let h = stem.rsplit('-').next()?;
    (h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| h.to_string())
}

/// Extract + gzip a raw session log; the stored name carries the session end time and the raw hash.
fn session_extract_gz(text: &str) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(mtgctl::mtgolog::decklist::session_extract(text).as_bytes()).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

async fn me(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<serde_json::Value>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let known = player_dir(&st, &name).logs().exists();
    Ok(Json(serde_json::json!({ "name": name, "known": known })))
}

/// Browser-side failures, written to the server log verbatim (bounded), for debugging what a page saw.
async fn client_log(body: Bytes) -> StatusCode {
    let text = String::from_utf8_lossy(&body[..body.len().min(4000)]).replace('\n', " ");
    eprintln!("client-log: {text}");
    StatusCode::NO_CONTENT
}

/// The reference decklists, read-only, with cards when `?id=` is given.
async fn reference_api(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Result<Json<serde_json::Value>, ApiError> {
    let data = st.data.clone();
    let id: Option<i32> = q.get("id").and_then(|s| s.parse().ok());
    let v = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let conn = &mut shared_db(&data)?;
        Ok(match id {
            Some(id) => serde_json::to_value(mtgctl::mtgolog::reference::cards(conn, id).map_err(|e| e.to_string())?).unwrap(),
            None => serde_json::to_value(mtgctl::mtgolog::reference::list(conn).map_err(|e| e.to_string())?).unwrap(),
        })
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    Ok(Json(v))
}

/// Every player with data on this server.
async fn players(State(st): State<Shared>) -> Json<Vec<String>> {
    let mut out: Vec<String> = std::fs::read_dir(st.data.join("players")).map(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
    out.sort();
    Json(out)
}

/// `file` parts holding a few game logs → the username they name as "me".
async fn detect(mut mp: Multipart) -> Result<Json<serde_json::Value>, ApiError> {
    let mut logs: Vec<Vec<u8>> = Vec::new();
    while let Some(field) = mp.next_field().await.map_err(|e| bad(e.to_string()))? {
        if field.name() == Some("file") && logs.len() < 20 {
            logs.push(field.bytes().await.map_err(|e| bad(e.to_string()))?.to_vec());
        }
    }
    match detect_me(&logs) {
        Some((name, n, parsed)) if valid_player(&name) => Ok(Json(serde_json::json!({ "name": name, "matches": n, "parsed": parsed }))),
        Some((name, ..)) => Err(bad(format!("detected username {name:?} is not usable as a player name"))),
        None => Err(bad("no parseable Match_GameLog files in the sample")),
    }
}

async fn manifest(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Vec<StoredFile>>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let dir = player_dir(&st, &name);
    Ok(Json(tokio::task::spawn_blocking(move || stored_files(&dir)).await.map_err(|e| internal(e.to_string()))?))
}

#[derive(Serialize)]
struct UploadResult {
    stored: Vec<String>,
    skipped: Vec<String>,
}

/// Multipart parts, repeated per file: `kind` (game|session|session-extract), `name`, `mtime`
/// (ms since epoch), `sha256` (session-extract only: hash of the raw session log), `file`.
/// Game logs keep their name (the match guid) and the newest copy wins. Session logs are reduced
/// to a gzipped extract named `mtgo-YYYYmmdd-HHMMSS-<sha256 of the raw log>.log.gz`, deduplicated
/// by that hash; `session-extract` is the same thing already reduced and gzipped by the collector.
/// The timestamp is the session's end, taken from an archive-style name when there is one (a copy's
/// mtime is the copy time, not the session's) and from the upload's mtime for a live `mtgo.log`.
async fn upload(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, mut mp: Multipart) -> Result<Json<UploadResult>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let dir = player_dir(&st, &name);
    dir.ensure().map_err(|e| internal(e.to_string()))?;
    let mut used: u64 = stored_files(&dir).iter().map(|f| f.size).sum();
    let mut res = UploadResult { stored: vec![], skipped: vec![] };
    let (mut kind, mut fname, mut mtime, mut sha): (Option<String>, Option<String>, Option<u64>, Option<String>) = (None, None, None, None);
    // the client's UTC offset in minutes, JavaScript's getTimezoneOffset sign (UTC minus local; 420 for UTC-7)
    let mut tz: Option<i64> = None;
    let existing_session_hashes: std::collections::HashSet<String> = stored_files(&dir).into_iter().filter(|f| f.kind == "session").map(|f| f.sha256).collect();
    let mut session_hashes = existing_session_hashes;
    while let Some(field) = mp.next_field().await.map_err(|e| bad(e.to_string()))? {
        match field.name().unwrap_or("") {
            "kind" => kind = Some(field.text().await.map_err(|e| bad(e.to_string()))?),
            "name" => fname = Some(field.text().await.map_err(|e| bad(e.to_string()))?),
            "mtime" => mtime = field.text().await.map_err(|e| bad(e.to_string()))?.trim().parse().ok(),
            "sha256" => sha = Some(field.text().await.map_err(|e| bad(e.to_string()))?.trim().to_lowercase()),
            "tz" => tz = field.text().await.map_err(|e| bad(e.to_string()))?.trim().parse().ok(),
            "file" => {
                let bytes: Bytes = field.bytes().await.map_err(|e| bad(e.to_string()))?;
                let k = kind.take().ok_or_else(|| bad("kind before file"))?;
                let n = fname.take().ok_or_else(|| bad("name before file"))?;
                let mt = mtime.take().unwrap_or(0);
                let s = sha.take();
                if used + bytes.len() as u64 > MAX_PLAYER_BYTES {
                    return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, format!("{name} is over the {} MB storage cap", MAX_PLAYER_BYTES >> 20)));
                }
                match store_file(&dir, &k, &n, mt, tz, s.as_deref(), &bytes, &mut session_hashes) {
                    Ok(Some(stored)) => {
                        used += bytes.len() as u64;
                        res.stored.push(stored)
                    }
                    Ok(None) => res.skipped.push(n),
                    Err(e) => return Err(bad(format!("{n}: {e}"))),
                }
            }
            other => return Err(bad(format!("unexpected field {other}"))),
        }
    }
    Ok(Json(res))
}

fn store_file(dir: &PlayerDir, kind: &str, name: &str, mtime_ms: u64, tz_min: Option<i64>, raw_sha: Option<&str>, bytes: &[u8], session_hashes: &mut std::collections::HashSet<String>) -> Result<Option<String>, String> {
    let base = Path::new(name).file_name().and_then(|n| n.to_str()).ok_or("bad name")?;
    let target = match kind {
        "game" => {
            let ok = base.starts_with("Match_GameLog_") && base.ends_with(".dat") && base.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
            if !ok {
                return Err("not a Match_GameLog_*.dat name".into());
            }
            // never store chat: refuse chat files whatever their name, drop chat records from a game log
            match mtgctl::mtgolog::format::strip_chat(bytes) {
                Ok(None) => {}
                Ok(Some(clean)) => return store_file(dir, kind, name, mtime_ms, tz_min, raw_sha, &clean, session_hashes),
                Err(e) => return Err(format!("not a game log ({e})")),
            }
            let p = dir.logs().join(base);
            if let Ok(md) = std::fs::metadata(&p) {
                let old_mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
                if md.len() >= bytes.len() as u64 && old_mtime >= mtime_ms {
                    return Ok(None);
                }
            }
            p
        }
        "session" | "session-extract" => {
            let h = match kind {
                "session-extract" => {
                    let h = raw_sha.ok_or("session-extract needs the raw log's sha256")?.to_string();
                    if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                        return Err("bad sha256".into());
                    }
                    if bytes.len() < 2 || bytes[0] != 0x1f || bytes[1] != 0x8b {
                        return Err("session-extract must be gzip".into());
                    }
                    h
                }
                _ => hex(&Sha256::digest(bytes)),
            };
            // The file name carries the session's end in the player's local time: the log has no dates of
            // its own, the parser works back from this day. An archive name (mtgo-YYYYMMDD-HHMMSS.log) is
            // already local; otherwise the modification time shifted by the client's offset, and only
            // without one the server's clock.
            let named = base.strip_prefix("mtgo-").and_then(|s| s.get(..15)).and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y%m%d-%H%M%S").ok());
            let local: chrono::NaiveDateTime = named.unwrap_or_else(|| {
                let utc = chrono::DateTime::<chrono::Utc>::from(UNIX_EPOCH + Duration::from_millis(mtime_ms));
                match tz_min {
                    Some(off) => (utc - chrono::Duration::minutes(off)).naive_utc(),
                    None => utc.with_timezone(&chrono::Local).naive_local(),
                }
            });
            let stamp = local.format("%Y%m%d-%H%M%S").to_string();
            let target = dir.sessions().join(format!("mtgo-{stamp}-{h}.log.gz"));
            if !session_hashes.insert(h.clone()) {
                // already stored: a client that now knows the local time corrects an earlier name
                if let Some(old) = std::fs::read_dir(dir.sessions()).ok().and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.to_string_lossy().ends_with(&format!("-{h}.log.gz")) && *p != target)) {
                    std::fs::rename(&old, &target).map_err(|e| e.to_string())?;
                    set_mtime(&target, local.and_utc().timestamp_millis().max(0) as u64)?;
                    return Ok(Some(target.file_name().unwrap().to_string_lossy().to_string()));
                }
                return Ok(None);
            }
            let gz = if kind == "session-extract" { bytes.to_vec() } else { session_extract_gz(&String::from_utf8_lossy(bytes))? };
            std::fs::write(&target, gz).map_err(|e| e.to_string())?;
            set_mtime(&target, local.and_utc().timestamp_millis().max(0) as u64)?;
            return Ok(Some(target.file_name().unwrap().to_string_lossy().to_string()));
        }
        _ => return Err("kind must be game or session".into()),
    };
    std::fs::write(&target, bytes).map_err(|e| e.to_string())?;
    set_mtime(&target, mtime_ms)?;
    Ok(Some(target.file_name().unwrap().to_string_lossy().to_string()))
}

fn set_mtime(target: &Path, mtime_ms: u64) -> Result<(), String> {
    if mtime_ms > 0 {
        let f = std::fs::File::options().write(true).open(target).map_err(|e| e.to_string())?;
        f.set_modified(UNIX_EPOCH + Duration::from_millis(mtime_ms)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// One-time: turn raw `sessions/*.log` files (from before extraction) into extracts and delete them.
fn compact_sessions(data: &Path) -> Result<(), String> {
    let players = data.join("players");
    let Ok(rd) = std::fs::read_dir(&players) else { return Ok(()) };
    for p in rd.flatten() {
        let sessions = p.path().join("sessions");
        let Ok(files) = std::fs::read_dir(&sessions) else { continue };
        let (mut n, mut before, mut after) = (0usize, 0u64, 0u64);
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".log") else { continue };
            let bytes = std::fs::read(f.path()).map_err(|e| e.to_string())?;
            let h = hex(&Sha256::digest(&bytes));
            let ts = stem.strip_prefix("mtgo-").and_then(|s| s.get(..15)).unwrap_or(stem);
            let gz = session_extract_gz(&String::from_utf8_lossy(&bytes))?;
            let target = sessions.join(format!("mtgo-{ts}-{h}.log.gz"));
            std::fs::write(&target, &gz).map_err(|e| e.to_string())?;
            let mt = f.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
            set_mtime(&target, mt)?;
            std::fs::remove_file(f.path()).map_err(|e| e.to_string())?;
            n += 1;
            before += bytes.len() as u64;
            after += gz.len() as u64;
        }
        println!("{}: {n} session logs compacted, {:.1} MB → {:.2} MB", p.file_name().to_string_lossy(), before as f64 / 1e6, after as f64 / 1e6);
    }
    Ok(())
}

/// Sync every player into the pooled database. Marks made in an older build's per-player database
/// (list name, fork and experimental tag by list hash) are carried onto lists that have none.
fn rebuild(data: &Path, reference: &Reference) -> Result<(), String> {
    use diesel::prelude::*;
    use diesel::sql_types::{Bool, Nullable, Text};
    #[derive(diesel::QueryableByName)]
    struct ListMark {
        #[diesel(sql_type = Text)]
        hash: String,
        #[diesel(sql_type = Nullable<Text>)]
        name: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        fork: Option<String>,
        #[diesel(sql_type = Bool)]
        experimental: bool,
    }
    let o = |v: &Option<String>| v.as_deref().map(sq).unwrap_or("NULL".into());
    let conn = &mut pipeline::connect(data)?;
    mtgctl::mtgolog::db::ensure_schema(conn).map_err(|e| e.to_string())?;
    stats::ensure_view(conn).map_err(|e| e.to_string())?;
    let mut names: Vec<String> = std::fs::read_dir(data.join("players")).map_err(|e| e.to_string())?.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    for name in names {
        let dir = player_dir_at(data, &name);
        let marks: Vec<ListMark> = if dir.old_db().exists() {
            let old = &mut mtgctl::db::establish_connection_at(&dir.old_db())?;
            diesel::sql_query("SELECT hash, name, fork, experimental FROM mtgo_lists WHERE name IS NOT NULL OR fork IS NOT NULL OR experimental = 1").load(old).map_err(|e| e.to_string())?
        } else {
            vec![]
        };
        let report = pipeline::sync(conn, &dir, &name, reference)?;
        let me = sq(&name);
        let mut carried = 0;
        for m in &marks {
            carried += diesel::sql_query(format!(
                "UPDATE mtgo_lists SET name = {}, fork = {}, experimental = {} WHERE me = {me} AND hash = {} AND name IS NULL AND fork IS NULL AND experimental = 0",
                o(&m.name), o(&m.fork), m.experimental as i32, sq(&m.hash)
            ))
            .execute(conn)
            .map_err(|e| e.to_string())?;
        }
        println!("{name}: {} matches imported, {} lists; {carried} of {} list marks carried over", report.imported, report.lists, marks.len());
        for step in &report.steps {
            println!("  {step}");
        }
    }
    Ok(())
}

async fn run_sync(st: Shared, name: String) -> Result<mtgctl::mtgolog::pipeline::SyncReport, ApiError> {
    let _g = st.sync_lock.lock().await;
    let st2 = st.clone();
    let report = tokio::task::spawn_blocking(move || {
        let dir = player_dir(&st2, &name);
        // the directory's name must be the username the logs themselves name as "me"
        let sample: Vec<Vec<u8>> = std::fs::read_dir(dir.logs()).map(|rd| rd.flatten().take(40).filter_map(|e| std::fs::read(e.path()).ok()).collect()).unwrap_or_default();
        if let Some((me, ..)) = detect_me(&sample) {
            if !me.eq_ignore_ascii_case(&name) {
                return Err(format!("these logs belong to {me}, not {name}"));
            }
        }
        pipeline::sync(&mut pool(&st2)?, &dir, &name, &st2.reference)
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    Ok(report)
}

async fn sync(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<mtgctl::mtgolog::pipeline::SyncReport>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    Ok(Json(run_sync(st, name).await?))
}

#[derive(Deserialize)]
struct StatsParams {
    #[serde(default)]
    by: String,
    #[serde(default, rename = "where")]
    where_sql: Option<String>,
    #[serde(default = "one")]
    min: i64,
    #[serde(default = "sixty")]
    limit: usize,
    #[serde(default = "games")]
    sort: String,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    hist: String,
}
fn one() -> i64 {
    1
}
fn sixty() -> usize {
    60
}
fn games() -> String {
    "games".into()
}

fn split_cols(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|c| !c.is_empty()).map(String::from).collect()
}

async fn stats_api(State(st): State<Shared>, Query(p): Query<StatsParams>) -> Result<Json<stats::StatsResult>, ApiError> {
    let query = stats::StatsQuery { by: split_cols(&p.by), per: None, where_sql: p.where_sql.filter(|w| !w.trim().is_empty()), min_games: p.min, sort: p.sort, limit: p.limit, full: p.full, hist: split_cols(&p.hist) };
    let r = tokio::task::spawn_blocking(move || {
        let conn = &mut pool(&st)?;
        stats::compute(conn, &query)
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(r))
}

async fn columns(State(st): State<Shared>) -> Result<Json<Vec<String>>, ApiError> {
    let cols = tokio::task::spawn_blocking(move || {
        let conn = &mut pool(&st)?;
        picker::view_columns(conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    Ok(Json(cols))
}

#[derive(Serialize)]
struct ValueRow {
    value: Option<String>,
    matches: i64,
    games: i64,
}

/// Distinct values of one view column under an optional WHERE, most matches first.
#[derive(Deserialize)]
struct ExperimentalReq {
    id: i32,
    on: bool,
}

/// Tag one of the player's registered lists as an experimental brew, or clear the tag.
async fn list_experimental(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, Json(req): Json<ExperimentalReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    tokio::task::spawn_blocking(move || {
        let conn = &mut pool(&st)?;
        mtgctl::mtgolog::decklist::set_experimental(conn, &name, req.id, req.on)
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(serde_json::json!({ "id": req.id, "experimental": req.on })))
}

/// Every card either side has revealed, for the card filters' suggestions.
async fn cards_seen(State(st): State<Shared>) -> Result<Json<Vec<String>>, ApiError> {
    let rows = tokio::task::spawn_blocking(move || {
        use diesel::prelude::*;
        let conn = &mut pool(&st)?;
        #[derive(diesel::QueryableByName)]
        struct R {
            #[diesel(sql_type = diesel::sql_types::Text)]
            name: String,
        }
        let rows: Vec<R> = diesel::sql_query("SELECT DISTINCT name FROM mtgo_game_cards WHERE played = 1 ORDER BY name").load(conn).map_err(|e| e.to_string())?;
        Ok::<_, String>(rows.into_iter().map(|r| r.name).collect::<Vec<_>>())
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    Ok(Json(rows))
}

async fn values(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Result<Json<Vec<ValueRow>>, ApiError> {
    let col = q.get("col").cloned().ok_or_else(|| bad("col required"))?;
    if !col.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(bad("bad column"));
    }
    let wher: Vec<String> = q.get("where").filter(|w| !w.trim().is_empty()).cloned().into_iter().collect();
    let rows = tokio::task::spawn_blocking(move || {
        let conn = &mut pool(&st)?;
        picker::column_values(conn, &col, &wher).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(rows.into_iter().map(|(value, matches, games)| ValueRow { value, matches, games }).collect()))
}

#[derive(diesel::QueryableByName, Serialize)]
struct ListRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    name: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    experimental: bool,
    #[diesel(sql_type = diesel::sql_types::Text)]
    first_seen: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    last_seen: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    matches: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    deck: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    archetype: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    deck_score: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    runner_up_deck: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    runner_up_dist: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    colors: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    reference_id: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    reference_name: Option<String>,
}

async fn lists(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Vec<ListRow>>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let rows = tokio::task::spawn_blocking(move || {
        use diesel::prelude::*;
        let conn = &mut pool(&st)?;
        diesel::sql_query(format!(
            "SELECT l.id, COALESCE(l.name, l.auto_name) AS name, l.experimental, l.first_seen, l.last_seen,
                    (SELECT COUNT(*) FROM mtgo_matches m WHERE m.my_list_id = l.id) AS matches,
                    a.deck, a.archetype, a.dist_main AS deck_score, a.runner_up_deck, a.runner_up_dist, a.colors,
                    a.reference_id, (SELECT r.name FROM shared.reference_decklists r WHERE r.id = a.reference_id) AS reference_name
             FROM mtgo_lists l LEFT JOIN list_assignments a ON a.list_id = l.id WHERE l.me = {} ORDER BY l.first_seen, l.id",
            sq(&name)
        ))
        .load::<ListRow>(conn)
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    Ok(Json(rows))
}

#[derive(diesel::QueryableByName, Serialize)]
struct ListCard {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    qty: i32,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    sideboard: bool,
}

#[derive(Serialize)]
struct ListDetail {
    id: i32,
    name: Option<String>,
    experimental: bool,
    first_seen: String,
    last_seen: String,
    matches: i64,
    deck: Option<String>,
    archetype: Option<String>,
    deck_score: Option<i32>,
    runner_up_deck: Option<String>,
    runner_up_dist: Option<i32>,
    colors: Option<String>,
    reference_id: Option<i32>,
    reference_name: Option<String>,
    cards: Vec<ListCard>,
    /// vs the assigned reference: (name, sideboard, qty in the reference, qty here), only where they differ
    diff: Vec<(String, bool, i32, i32)>,
}

/// One registered list with its cards and the card-level delta against its assigned reference.
async fn list_detail(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<ListDetail>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let id: i32 = q.get("id").and_then(|s| s.parse().ok()).ok_or_else(|| bad("id required"))?;
    let r = tokio::task::spawn_blocking(move || -> Result<ListDetail, String> {
        use diesel::prelude::*;
        let conn = &mut pool(&st)?;
        let rows: Vec<ListRow> = diesel::sql_query(format!(
            "SELECT l.id, COALESCE(l.name, l.auto_name) AS name, l.experimental, l.first_seen, l.last_seen,
                    (SELECT COUNT(*) FROM mtgo_matches m WHERE m.my_list_id = l.id) AS matches,
                    a.deck, a.archetype, a.dist_main AS deck_score, a.runner_up_deck, a.runner_up_dist, a.colors,
                    a.reference_id, (SELECT r.name FROM shared.reference_decklists r WHERE r.id = a.reference_id) AS reference_name
             FROM mtgo_lists l LEFT JOIN list_assignments a ON a.list_id = l.id WHERE l.id = {id} AND l.me = {}",
            sq(&name)
        ))
        .load(conn)
        .map_err(|e| e.to_string())?;
        let l = rows.into_iter().next().ok_or_else(|| format!("no list {id}"))?;
        let load = |conn: &mut diesel::SqliteConnection, id: i32| -> Result<Vec<ListCard>, String> {
            diesel::sql_query(format!("SELECT name, qty, sideboard FROM mtgo_list_cards WHERE list_id = {id} ORDER BY sideboard, name")).load(conn).map_err(|e| e.to_string())
        };
        let cards = load(conn, id)?;
        let mut diff = Vec::new();
        if let Some(rid) = l.reference_id {
            let parent: Vec<ListCard> = diesel::sql_query(format!("SELECT name, qty, sideboard FROM shared.reference_cards WHERE reference_id = {rid} ORDER BY sideboard, name")).load(conn).map_err(|e| e.to_string())?;
            let mut keys: std::collections::BTreeSet<(bool, String)> = cards.iter().map(|c| (c.sideboard, c.name.clone())).collect();
            keys.extend(parent.iter().map(|c| (c.sideboard, c.name.clone())));
            for (sb, n) in keys {
                let a = parent.iter().find(|c| c.sideboard == sb && c.name == n).map_or(0, |c| c.qty);
                let b = cards.iter().find(|c| c.sideboard == sb && c.name == n).map_or(0, |c| c.qty);
                if a != b {
                    diff.push((n, sb, a, b));
                }
            }
        }
        Ok(ListDetail { id: l.id, name: l.name, experimental: l.experimental, first_seen: l.first_seen, last_seen: l.last_seen, matches: l.matches, deck: l.deck, archetype: l.archetype, deck_score: l.deck_score, runner_up_deck: l.runner_up_deck, runner_up_dist: l.runner_up_dist, colors: l.colors, reference_id: l.reference_id, reference_name: l.reference_name, cards, diff })
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(r))
}

#[derive(diesel::QueryableByName, Serialize)]
struct Summary {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    matches: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    latest_match: Option<String>,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    lists: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    latest_list: Option<String>,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    matches_with_list: i64,
}

/// What the server already knows for a player, in the terms the upload page speaks.
async fn summary(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Summary>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let r = tokio::task::spawn_blocking(move || -> Result<Summary, String> {
        use diesel::prelude::*;
        let conn = &mut pool(&st)?;
        let me = sq(&name);
        diesel::sql_query(format!(
            "SELECT (SELECT COUNT(*) FROM mtgo_matches WHERE replay = 0 AND me = {me}) AS matches,
                    (SELECT MAX(start_ts) FROM mtgo_matches WHERE replay = 0 AND me = {me}) AS latest_match,
                    (SELECT COUNT(*) FROM mtgo_lists WHERE me = {me}) AS lists,
                    (SELECT COALESCE(name, auto_name) FROM mtgo_lists WHERE me = {me} ORDER BY last_seen DESC LIMIT 1) AS latest_list,
                    (SELECT COUNT(*) FROM mtgo_matches WHERE replay = 0 AND my_list_id IS NOT NULL AND me = {me}) AS matches_with_list"
        ))
        .get_result(conn)
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    Ok(Json(r))
}

#[derive(diesel::QueryableByName, Serialize)]
struct MatchRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    match_id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    date: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    start: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opponent: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    my_list: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    my_deck: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    my_archetype: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    my_deck_source: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_deck: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_archetype: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_deck_candidates: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_candidate_posteriors: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
    opp_posterior: Option<f64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
    opp_surprise: Option<f64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
    opp_archetype_surprise: Option<f64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_misfit: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_seen: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_colors: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    event_name: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Text)]
    match_result: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    my_game_wins: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    opp_game_wins: i32,
    /// per game, in order: "id:g:pd:mulls:turns:winner:win_by" — compact, rendered by the page
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    games: Option<String>,
}

#[derive(Deserialize)]
struct MatchesParams {
    #[serde(default, rename = "where")]
    where_sql: Option<String>,
    #[serde(default = "two_hundred")]
    limit: usize,
}
fn two_hundred() -> usize {
    200
}

/// The matches a WHERE over mtgo_game_view selects (a match is included if any of its games
/// passes), newest first, with a compact per-game summary.
async fn matches_api(State(st): State<Shared>, Query(p): Query<MatchesParams>) -> Result<Json<Vec<MatchRow>>, ApiError> {
    let wher = p.where_sql.filter(|w| !w.trim().is_empty()).map(|w| format!("WHERE {w}")).unwrap_or_default();
    let limit = p.limit.min(2000);
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<MatchRow>, String> {
        use diesel::prelude::*;
        let conn = &mut pool(&st)?;
        diesel::sql_query(format!(
            "SELECT v.match_id, v.date, v.match_start AS start, v.opponent, v.my_list, v.my_deck, v.my_archetype, v.my_deck_source, v.opp_deck, v.opp_archetype, v.opp_deck_candidates, v.opp_candidate_posteriors, v.opp_posterior, v.opp_surprise, v.opp_archetype_surprise, v.opp_misfit, v.opp_seen, v.opp_colors, v.event_name, v.match_result, v.my_game_wins, v.opp_game_wins,
                    (SELECT GROUP_CONCAT(g.id || ':' || g.game_number || ':' || CASE g.on_play WHEN 1 THEN 'play' ELSE 'draw' END || ':' || g.my_mulligans || ':' || g.turns || ':' || COALESCE(g.winner, '?') || ':' || COALESCE(g.win_by, ''), '|')
                     FROM (SELECT * FROM mtgo_games WHERE match_id = v.match_id ORDER BY game_number) g) AS games
             FROM mtgo_game_view v
             {wher}
             GROUP BY v.match_id
             ORDER BY v.match_start DESC, v.match_id DESC
             LIMIT {limit}"
        ))
        .load(conn)
        .map_err(|e| format!("query failed: {e}"))
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(rows))
}

#[derive(diesel::QueryableByName, Serialize)]
struct EventRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    seq: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    turn: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    side: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Text)]
    kind: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    card: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Text)]
    text: String,
}

#[derive(diesel::QueryableByName, Serialize)]
struct GameMeta {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    match_id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    game_number: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Bool>)]
    on_play: Option<bool>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    turns: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    winner: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    win_by: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    my_mulligans: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    opp_mulligans: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opponent: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Text)]
    start_ts: String,
}

#[derive(diesel::QueryableByName, Serialize)]
struct PlayedCard {
    #[diesel(sql_type = diesel::sql_types::Text)]
    side: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    plays: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    first_turn: Option<i32>,
}

#[derive(Serialize)]
struct GameDetail {
    game: GameMeta,
    cards: Vec<PlayedCard>,
    events: Vec<EventRow>,
}

/// Strip MTGO's `@[Name@:catalog,instance:@]` card markup down to the name.
fn plain_text(s: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"@\[([^@]*)@:[0-9,]*:@\]").unwrap());
    RE.replace_all(s, "$1").into_owned()
}

/// One game: its facts, the cards each side played (with first turn), and the event log.
async fn game_detail(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Result<Json<GameDetail>, ApiError> {
    let id: i32 = q.get("id").and_then(|s| s.parse().ok()).ok_or_else(|| bad("id required"))?;
    let r = tokio::task::spawn_blocking(move || -> Result<GameDetail, String> {
        use diesel::prelude::*;
        let conn = &mut pool(&st)?;
        let game: GameMeta = diesel::sql_query(format!("SELECT g.id, g.match_id, g.game_number, g.on_play, g.turns, g.winner, g.win_by, g.my_mulligans, g.opp_mulligans, m.opponent, g.start_ts FROM mtgo_games g JOIN mtgo_matches m ON m.id = g.match_id WHERE g.id = {id}")).get_result(conn).map_err(|e| format!("no game {id}: {e}"))?;
        let cards: Vec<PlayedCard> = diesel::sql_query(format!("SELECT side, name, plays, first_turn FROM mtgo_game_cards WHERE game_id = {id} AND played = 1 ORDER BY side, first_turn, name")).load(conn).map_err(|e| e.to_string())?;
        let mut events: Vec<EventRow> = diesel::sql_query(format!("SELECT seq, turn, side, kind, card, text FROM mtgo_events WHERE game_id = {id} ORDER BY seq")).load(conn).map_err(|e| e.to_string())?;
        for e in &mut events {
            e.text = plain_text(&e.text);
        }
        Ok(GameDetail { game, cards, events })
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(r))
}

#[derive(diesel::QueryableByName, Serialize)]
struct GameRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    game_id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    match_id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    date: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    start: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opponent: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_deck: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_archetype: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_deck_candidates: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_candidate_posteriors: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
    opp_posterior: Option<f64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
    opp_surprise: Option<f64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Double>)]
    opp_archetype_surprise: Option<f64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_misfit: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_seen: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_colors: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    my_list: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    my_deck: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    event_name: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    game_number: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Bool>)]
    on_play: Option<bool>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    my_mulligans: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    opp_mulligans: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    turns: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    winner: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    win_by: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    minutes: Option<i64>,
}

/// One row per game the WHERE selects, newest first.
async fn games_api(State(st): State<Shared>, Query(p): Query<MatchesParams>) -> Result<Json<Vec<GameRow>>, ApiError> {
    let wher = p.where_sql.filter(|w| !w.trim().is_empty()).map(|w| format!("WHERE {w}")).unwrap_or_default();
    let limit = p.limit.min(5000);
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<GameRow>, String> {
        use diesel::prelude::*;
        let conn = &mut pool(&st)?;
        diesel::sql_query(format!(
            "SELECT game_id, match_id, date, game_start AS start, opponent, opp_deck, opp_archetype, opp_deck_candidates, opp_candidate_posteriors, opp_posterior, opp_surprise, opp_archetype_surprise, opp_misfit, opp_seen, opp_colors, my_list, my_deck, event_name, game_number, on_play, my_mulligans, opp_mulligans, turns, winner, win_by, minutes
             FROM mtgo_game_view {wher} ORDER BY game_start DESC, game_id DESC LIMIT {limit}"
        ))
        .load(conn)
        .map_err(|e| format!("query failed: {e}"))
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(rows))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Cmd::RefAdd { archetype, deck, name, date, source, by, file } => {
            let text = std::fs::read_to_string(&file).unwrap_or_else(|e| { eprintln!("{}: {e}", file.display()); std::process::exit(1) });
            let conn = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let eras = timeline_eras();
            match mtgctl::mtgolog::reference::add(conn, &mtgctl::mtgolog::reference::NewReference { archetype: &archetype, deck: &deck, name: &name, date: &date, source: source.as_deref(), added_by: by.as_deref(), text: &text }, &eras) {
                Ok(id) => { let era = mtgctl::mtgolog::eras::era_for(&eras, &date).map(|(_, n)| n.as_str()).unwrap_or("?"); println!("reference decklist #{id}: {archetype} / {deck} — {name} (era: {era})") }
                Err(e) => { eprintln!("{e}"); std::process::exit(1) }
            }
        }
        Cmd::SessionRemove { player, name } => {
            let p = cli.data.join("players").join(&player).join("sessions").join(Path::new(&name).file_name().unwrap_or_default());
            match std::fs::remove_file(&p) { Ok(()) => println!("removed {}", p.display()), Err(e) => { eprintln!("{}: {e}", p.display()); std::process::exit(1) } }
        }
        Cmd::RefMerge { deck, into } => {
            let conn = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            match mtgctl::mtgolog::reference::merge_deck(conn, &deck, &into) { Ok(n) => println!("{deck} → {into}: {n} references moved"), Err(e) => { eprintln!("{e}"); std::process::exit(1) } }
        }
        Cmd::RefMove { deck, archetype } => {
            let conn = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            match mtgctl::mtgolog::reference::move_deck(conn, &deck, &archetype) { Ok(()) => println!("{deck} → {archetype}"), Err(e) => { eprintln!("{e}"); std::process::exit(1) } }
        }
        Cmd::RefReera => {
            let conn = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            match mtgctl::mtgolog::reference::reassign_eras(conn, &timeline_eras()) { Ok(n) => println!("{n} references re-placed in eras"), Err(e) => eprintln!("{e}") }
        }
        Cmd::RefList => {
            let conn = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            match mtgctl::mtgolog::reference::list(conn) {
                Ok(rows) => for r in rows { println!("#{:<3} {:<18} {:<26} {:<30} {}/{}  {}", r.id, r.archetype, r.deck, r.era, r.n_main, r.n_side, r.name); },
                Err(e) => eprintln!("{e}"),
            }
        }
        Cmd::RefRemove { id } => {
            let conn = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            match mtgctl::mtgolog::reference::remove(conn, id) { Ok(1) => println!("removed #{id}"), Ok(_) => println!("no reference #{id}"), Err(e) => eprintln!("{e}") }
        }
        Cmd::CardsImport { file } => {
            let conn = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            match mtgctl::mtgolog::reference::import_card_facts(conn, &file) { Ok(n) => println!("{n} cards"), Err(e) => { eprintln!("{e}"); std::process::exit(1) } }
        }
        Cmd::Infer { player, side, pool_run, .. } => {
            use diesel::prelude::*;
            use mtgctl::mtgolog::reference as r;
            let shared = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let refs = r::load_references(shared, &timeline_eras()).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let conn = &mut pipeline::connect(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            #[derive(diesel::QueryableByName)]
            struct Truth { #[diesel(sql_type = diesel::sql_types::Integer)] match_id: i32, #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)] deck: Option<String>, #[diesel(sql_type = diesel::sql_types::Text)] date: String }
            let truth: Vec<Truth> = diesel::sql_query(format!("SELECT m.id AS match_id, a.deck, COALESCE(substr(m.start_ts,1,10), '') AS date FROM mtgo_matches m LEFT JOIN list_assignments a ON a.list_id = m.my_list_id WHERE m.replay = 0 AND m.me = {} ORDER BY m.start_ts", sq(&player))).load(conn).unwrap();
            let mut games = r::games_by_match(conn, &side, &refs.fetches, &player).unwrap();
            for gs in games.values_mut() {
                r::cap_revealed(gs, &refs);
            }
            if pool_run {
                games = r::pool_runs(&truth.iter().map(|t| (t.match_id, t.date.clone())).collect::<Vec<_>>(), &games, &refs).into_iter().map(|(k, (g, _))| (k, g)).collect();
            }
            if side == "me" {
                // list-level check: with the player's lists as hypotheses, does a match linked by the session log land on its own list?
                let lists = r::player_lists(conn, &refs.fetches, &player).unwrap();
                let with_lists = refs.with_lists(&lists);
                #[derive(diesel::QueryableByName)]
                struct Linked { #[diesel(sql_type = diesel::sql_types::Integer)] match_id: i32, #[diesel(sql_type = diesel::sql_types::Integer)] list_id: i32, #[diesel(sql_type = diesel::sql_types::Text)] date: String }
                let linked: Vec<Linked> = diesel::sql_query("SELECT id AS match_id, my_list_id AS list_id, COALESCE(substr(start_ts,1,10), '') AS date FROM mtgo_matches WHERE my_list_id IS NOT NULL AND replay = 0").load(conn).unwrap();
                let (mut lr, mut lw, mut ln) = (0, 0, 0);
                let mut misses: Vec<String> = vec![];
                let by_id: HashMap<i32, &r::RefList> = lists.iter().map(|l| (-l.list.id, &l.list)).collect();
                for l in &linked {
                    let Some(gs) = games.get(&l.match_id) else { continue };
                    match r::infer_list(gs, &l.date, &with_lists) {
                        Some((id, _)) if id == l.list_id => lr += 1,
                        Some((id, p)) => {
                            lw += 1;
                            if let (Some(a), Some(b)) = (by_id.get(&l.list_id), by_id.get(&id)) {
                                let seen: std::collections::BTreeMap<String, i32> = gs.iter().flat_map(|g| g.revealed.iter()).map(|(n, q)| (n.clone(), *q)).collect();
                                let decisive: Vec<String> = seen.keys().filter(|c| (a.main.get(*c).copied().unwrap_or(0) + a.side.get(*c).copied().unwrap_or(0)) != (b.main.get(*c).copied().unwrap_or(0) + b.side.get(*c).copied().unwrap_or(0))).cloned().collect();
                                if misses.len() < 8 { misses.push(format!("match {} truth list#{} → list#{} at {:.2}: main dist {}, side dist {}, {} games, cards seen that differ: {}", l.match_id, l.list_id, id, p, r::distance(&a.main, &b.main), r::distance(&a.side, &b.side), gs.len(), decisive.join(", "))); }
                            }
                        }
                        None => ln += 1,
                    }
                }
                println!("linked matches placed on a registered list: right {lr}, wrong {lw}, no list {ln} of {}", lr + lw + ln);
                for m in misses { println!("  {m}"); }
            }
            let (mut right, mut wrong, mut amb, mut unfit, mut known) = (0, 0, 0, 0, 0);
            let mut confusion: std::collections::BTreeMap<(String, String), usize> = Default::default();
            let mut soft: std::collections::BTreeMap<String, usize> = Default::default();
            let mut examples: Vec<String> = vec![];
            for t in &truth {
                let Some(gs) = games.get(&t.match_id) else { continue };
                let inf = r::infer(gs, &refs);
                let got = inf.deck.clone().unwrap_or_else(|| if inf.candidates.is_empty() { format!("unfit (surprise {:.2})", inf.surprise) } else { format!("{} ({})", inf.archetype.clone().unwrap_or_else(|| "archetype ambiguous".into()), inf.candidates.iter().map(|(d, p)| format!("{d} {p:.2}")).collect::<Vec<_>>().join(", ")) });
                if side == "me" && t.deck.is_some() {
                    known += 1;
                    let truth_deck = t.deck.clone().unwrap();
                    if inf.deck.as_deref() == Some(truth_deck.as_str()) { right += 1 } else if inf.deck.is_some() { wrong += 1; if examples.len() < 8 { examples.push(format!("{} truth {} → {} (posterior {:.2}, surprise {:.2}, {} cards revealed, not in it: {})", t.date, truth_deck, got, inf.posterior, inf.surprise, inf.n_seen, inf.misfit.join(", "))); } } else if !inf.candidates.is_empty() { amb += 1 } else { unfit += 1 }
                    *confusion.entry((truth_deck, got)).or_default() += 1;
                } else {
                    *soft.entry(got.clone()).or_default() += 1;
                }
            }
            if side == "me" {
                println!("known-list matches: {known}: right {right}, wrong {wrong}, ambiguous {amb}, unfit {unfit}  (accuracy when named: {:.0}%)", if right + wrong > 0 { 100.0 * right as f64 / (right + wrong) as f64 } else { 0.0 });
                for ((t, g), n) in &confusion { println!("  truth {:<12} → {:<60} {n}", t, g); }
                for e in examples { println!("  miss: {e}"); }
            }
            println!("\n{} matches without a known list:", soft.values().sum::<usize>());
            for (g, n) in &soft { println!("  {:<60} {n}", g); }
        }
        Cmd::Assign { player } => {
            use mtgctl::mtgolog::reference as r;
            let shared = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let refs = r::load_references(shared, &timeline_eras()).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let conn = &mut pipeline::connect(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let names: HashMap<i32, String> = {
                use diesel::prelude::*;
                #[derive(diesel::QueryableByName)]
                struct N { #[diesel(sql_type = diesel::sql_types::Integer)] id: i32, #[diesel(sql_type = diesel::sql_types::Text)] name: String }
                diesel::sql_query(format!("SELECT id, COALESCE(name, auto_name, 'list#' || id) AS name FROM mtgo_lists WHERE me = {}", sq(&player))).load::<N>(conn).unwrap_or_default().into_iter().map(|n| (n.id, n.name)).collect()
            };
            match r::assign_player(conn, &refs, &player) {
                Ok(asg) => {
                    for r in &refs.refs { println!("reference #{} {:<12} colours {}", r.id, r.deck, r.colors); }
                    println!("{:<4} {:<58} {:<6} {:<12} {:>5} {:>4}  runner-up", "list", "name", "mana", "deck", "score", "side");
                    for a in asg {
                        println!("{:<4} {:<58} {:<6} {:<12} {:>5} {:>4}  {}", a.list_id, names.get(&a.list_id).map(|s| s.chars().take(58).collect::<String>()).unwrap_or_default(), a.colors, a.deck.clone().unwrap_or_else(|| "—".into()), a.dist_main, a.dist_side, a.runner_up_deck.map(|d| format!("{d} at {}", a.runner_up_dist.unwrap_or(0))).unwrap_or_default());
                    }
                }
                Err(e) => eprintln!("{e}"),
            }
        }
        Cmd::Rebuild { repo } => {
            if let Err(e) = rebuild(&cli.data, &Reference::locate(&repo, &cli.data)) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Cmd::PlayerRemove { player } => {
            let conn = &mut pipeline::connect(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            match mtgctl::mtgolog::db::ensure_schema(conn).and_then(|_| mtgctl::mtgolog::db::remove_player(conn, &player)) {
                Ok(n) => println!("{player}: {n} matches deleted"),
                Err(e) => { eprintln!("{e}"); std::process::exit(1) }
            }
            let dir = cli.data.join("players").join(&player);
            if dir.exists() {
                std::fs::remove_dir_all(&dir).unwrap_or_else(|e| { eprintln!("{}: {e}", dir.display()); std::process::exit(1) });
                println!("removed {}", dir.display());
            }
        }
        Cmd::CompactSessions => {
            if let Err(e) = compact_sessions(&cli.data) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Cmd::Serve { listen, web, repo } => {
            std::fs::create_dir_all(cli.data.join("players")).expect("data dir");
            // once here, so that requests only read
            let conn = &mut pipeline::connect(&cli.data).expect("database");
            mtgctl::mtgolog::db::ensure_schema(conn).expect("schema");
            stats::ensure_view(conn).expect("view");
            let state = Arc::new(AppState { data: cli.data.clone(), reference: Reference::locate(&repo, &cli.data), sync_lock: Mutex::new(()) });
            let app = Router::new()
                .route("/api/me", get(me))
                .route("/api/players", get(players))
                .route("/api/reference", get(reference_api))
                .route("/api/client-log", post(client_log))
                .route("/api/detect", post(detect))
                .route("/api/summary", get(summary))
                .route("/api/manifest", get(manifest))
                .route("/api/upload", post(upload))
                .route("/api/sync", post(sync))
                .route("/api/stats", get(stats_api))
                .route("/api/matches", get(matches_api))
                .route("/api/games", get(games_api))
                .route("/api/game", get(game_detail))
                .route("/api/columns", get(columns))
                .route("/api/values", get(values))
                .route("/api/cards-seen", get(cards_seen))
                .route("/api/list/experimental", post(list_experimental))
                .route("/api/lists", get(lists))
                .route("/api/list", get(list_detail))
                .fallback_service(ServeDir::new(&web))
                .layer(axum::middleware::from_fn(plain_text_scripts))
                // pages and scripts change together on every deploy; never serve a stale script against a new page
                .layer(SetResponseHeaderLayer::overriding(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-cache")))
                .layer(DefaultBodyLimit::max(1024 * 1024 * 1024))
                .with_state(state);
            let listener = tokio::net::TcpListener::bind(&listen).await.expect("bind");
            eprintln!("mtgctl-web on http://{listen} (data {}, web {})", cli.data.display(), web.display());
            axum::serve(listener, app).await.expect("serve");
        }
    }
}
