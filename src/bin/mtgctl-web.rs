//! HTTP front-end over the mtgolog pipeline: players upload their MTGO log files, the server
//! keeps one database per player and answers the same stats queries as `mtgctl mtgo stats`.
//! Identity (v0): the MTGO username the game logs themselves name as "me" — detected by
//! `/api/detect`, sent as the `X-Player` header, and re-checked against the stored logs on every
//! sync. No secrets; a real login (Discord) is the planned replacement.
//! Layout: `<data>/players/<name>/{logs,sessions,mtgctl.db}`.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use mtgctl::mtgolog::pipeline::{PlayerDir, Reference};
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
    Serve {
        #[arg(long, default_value = "127.0.0.1:8090")]
        listen: String,
        /// Static files (the stats and collector pages).
        #[arg(long, default_value = "web")]
        web: PathBuf,
        /// Repo root holding definitions/mtgo/markers.toml.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
}


struct AppState {
    data: PathBuf,
    reference: Reference,
    /// one pipeline run per player at a time
    sync_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
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

fn player_dir(state: &AppState, name: &str) -> PlayerDir {
    PlayerDir::new(state.data.join("players").join(name))
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
    let known = player_dir(&st, &name).db().exists();
    Ok(Json(serde_json::json!({ "name": name, "known": known })))
}

/// Browser-side failures, written to the server log verbatim (bounded), for debugging what a page saw.
async fn client_log(body: Bytes) -> StatusCode {
    let text = String::from_utf8_lossy(&body[..body.len().min(4000)]).replace('\n', " ");
    eprintln!("client-log: {text}");
    StatusCode::NO_CONTENT
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
    let existing_session_hashes: std::collections::HashSet<String> = stored_files(&dir).into_iter().filter(|f| f.kind == "session").map(|f| f.sha256).collect();
    let mut session_hashes = existing_session_hashes;
    while let Some(field) = mp.next_field().await.map_err(|e| bad(e.to_string()))? {
        match field.name().unwrap_or("") {
            "kind" => kind = Some(field.text().await.map_err(|e| bad(e.to_string()))?),
            "name" => fname = Some(field.text().await.map_err(|e| bad(e.to_string()))?),
            "mtime" => mtime = field.text().await.map_err(|e| bad(e.to_string()))?.trim().parse().ok(),
            "sha256" => sha = Some(field.text().await.map_err(|e| bad(e.to_string()))?.trim().to_lowercase()),
            "file" => {
                let bytes: Bytes = field.bytes().await.map_err(|e| bad(e.to_string()))?;
                let k = kind.take().ok_or_else(|| bad("kind before file"))?;
                let n = fname.take().ok_or_else(|| bad("name before file"))?;
                let mt = mtime.take().unwrap_or(0);
                let s = sha.take();
                if used + bytes.len() as u64 > MAX_PLAYER_BYTES {
                    return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, format!("{name} is over the {} MB storage cap", MAX_PLAYER_BYTES >> 20)));
                }
                match store_file(&dir, &k, &n, mt, s.as_deref(), &bytes, &mut session_hashes) {
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

fn store_file(dir: &PlayerDir, kind: &str, name: &str, mtime_ms: u64, raw_sha: Option<&str>, bytes: &[u8], session_hashes: &mut std::collections::HashSet<String>) -> Result<Option<String>, String> {
    let base = Path::new(name).file_name().and_then(|n| n.to_str()).ok_or("bad name")?;
    let target = match kind {
        "game" => {
            let ok = base.starts_with("Match_GameLog_") && base.ends_with(".dat") && base.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
            if !ok {
                return Err("not a Match_GameLog_*.dat name".into());
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
            if !session_hashes.insert(h.clone()) {
                return Ok(None);
            }
            let named = base
                .strip_prefix("mtgo-")
                .and_then(|s| s.get(..15))
                .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y%m%d-%H%M%S").ok())
                .and_then(|t| t.and_local_timezone(chrono::Local).single());
            let t = named.unwrap_or_else(|| chrono::DateTime::<chrono::Local>::from(UNIX_EPOCH + Duration::from_millis(mtime_ms)));
            let gz = if kind == "session-extract" { bytes.to_vec() } else { session_extract_gz(&String::from_utf8_lossy(bytes))? };
            let target = dir.sessions().join(format!("mtgo-{}-{}.log.gz", t.format("%Y%m%d-%H%M%S"), h));
            std::fs::write(&target, gz).map_err(|e| e.to_string())?;
            set_mtime(&target, t.timestamp_millis().max(0) as u64)?;
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

async fn run_sync(st: Shared, name: String) -> Result<mtgctl::mtgolog::pipeline::SyncReport, ApiError> {
    let lock = st.sync_locks.lock().await.entry(name.clone()).or_insert_with(|| Arc::new(Mutex::new(()))).clone();
    let _g = lock.lock().await;
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
        mtgctl::mtgolog::pipeline::sync(&dir, &name, &st2.reference)
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
    #[serde(default)]
    player: Option<String>,
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

async fn stats_api(State(st): State<Shared>, headers: HeaderMap, Query(p): Query<StatsParams>) -> Result<Json<stats::StatsResult>, ApiError> {
    let mut q = HashMap::new();
    if let Some(t) = &p.player {
        q.insert("player".to_string(), t.clone());
    }
    let name = auth(&st, &headers, &q)?;
    let query = stats::StatsQuery { by: split_cols(&p.by), per: None, where_sql: p.where_sql.filter(|w| !w.trim().is_empty()), min_games: p.min, sort: p.sort, limit: p.limit, full: p.full, hist: split_cols(&p.hist) };
    let dir = player_dir(&st, &name);
    let r = tokio::task::spawn_blocking(move || {
        let conn = &mut dir.connect()?;
        stats::compute(conn, &query)
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(r))
}

async fn columns(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Vec<String>>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let dir = player_dir(&st, &name);
    let cols = tokio::task::spawn_blocking(move || {
        let conn = &mut dir.connect()?;
        mtgctl::mtgolog::db::ensure_schema(conn).map_err(|e| e.to_string())?;
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
async fn values(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Vec<ValueRow>>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let col = q.get("col").cloned().ok_or_else(|| bad("col required"))?;
    if !col.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(bad("bad column"));
    }
    let wher: Vec<String> = q.get("where").filter(|w| !w.trim().is_empty()).cloned().into_iter().collect();
    let dir = player_dir(&st, &name);
    let rows = tokio::task::spawn_blocking(move || {
        let conn = &mut dir.connect()?;
        mtgctl::mtgolog::db::ensure_schema(conn).map_err(|e| e.to_string())?;
        stats::ensure_view(conn).map_err(|e| e.to_string())?;
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
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    concept: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    parent_id: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    experimental: bool,
    #[diesel(sql_type = diesel::sql_types::Text)]
    first_seen: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    last_seen: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    matches: i64,
}

async fn lists(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Vec<ListRow>>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let dir = player_dir(&st, &name);
    let rows = tokio::task::spawn_blocking(move || {
        use diesel::prelude::*;
        let conn = &mut dir.connect()?;
        mtgctl::mtgolog::db::ensure_schema(conn).map_err(|e| e.to_string())?;
        diesel::sql_query(
            "SELECT l.id, COALESCE(l.name, l.auto_name) AS name, l.concept, l.parent_id, l.experimental, l.first_seen, l.last_seen,
                    (SELECT COUNT(*) FROM mtgo_matches m WHERE m.my_list_id = l.id) AS matches
             FROM mtgo_lists l ORDER BY l.first_seen, l.id",
        )
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
    concept: Option<String>,
    parent_id: Option<i32>,
    experimental: bool,
    first_seen: String,
    last_seen: String,
    matches: i64,
    cards: Vec<ListCard>,
    /// vs the parent: (name, sideboard, qty in parent, qty here), only where they differ
    diff: Vec<(String, bool, i32, i32)>,
}

/// One registered list with its cards and the card-level delta against its parent.
async fn list_detail(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<ListDetail>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let id: i32 = q.get("id").and_then(|s| s.parse().ok()).ok_or_else(|| bad("id required"))?;
    let dir = player_dir(&st, &name);
    let r = tokio::task::spawn_blocking(move || -> Result<ListDetail, String> {
        use diesel::prelude::*;
        let conn = &mut dir.connect()?;
        mtgctl::mtgolog::db::ensure_schema(conn).map_err(|e| e.to_string())?;
        let rows: Vec<ListRow> = diesel::sql_query(format!(
            "SELECT l.id, COALESCE(l.name, l.auto_name) AS name, l.concept, l.parent_id, l.experimental, l.first_seen, l.last_seen,
                    (SELECT COUNT(*) FROM mtgo_matches m WHERE m.my_list_id = l.id) AS matches
             FROM mtgo_lists l WHERE l.id = {id}"
        ))
        .load(conn)
        .map_err(|e| e.to_string())?;
        let l = rows.into_iter().next().ok_or_else(|| format!("no list {id}"))?;
        let load = |conn: &mut diesel::SqliteConnection, id: i32| -> Result<Vec<ListCard>, String> {
            diesel::sql_query(format!("SELECT name, qty, sideboard FROM mtgo_list_cards WHERE list_id = {id} ORDER BY sideboard, name")).load(conn).map_err(|e| e.to_string())
        };
        let cards = load(conn, id)?;
        let mut diff = Vec::new();
        if let Some(p) = l.parent_id {
            let parent = load(conn, p)?;
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
        Ok(ListDetail { id: l.id, name: l.name, concept: l.concept, parent_id: l.parent_id, experimental: l.experimental, first_seen: l.first_seen, last_seen: l.last_seen, matches: l.matches, cards, diff })
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
    let dir = player_dir(&st, &name);
    if !dir.db().exists() {
        return Ok(Json(Summary { matches: 0, latest_match: None, lists: 0, latest_list: None, matches_with_list: 0 }));
    }
    let r = tokio::task::spawn_blocking(move || -> Result<Summary, String> {
        use diesel::prelude::*;
        let conn = &mut dir.connect()?;
        mtgctl::mtgolog::db::ensure_schema(conn).map_err(|e| e.to_string())?;
        diesel::sql_query(
            "SELECT (SELECT COUNT(*) FROM mtgo_matches WHERE replay = 0) AS matches,
                    (SELECT MAX(start_ts) FROM mtgo_matches WHERE replay = 0) AS latest_match,
                    (SELECT COUNT(*) FROM mtgo_lists) AS lists,
                    (SELECT COALESCE(name, auto_name) FROM mtgo_lists ORDER BY last_seen DESC LIMIT 1) AS latest_list,
                    (SELECT COUNT(*) FROM mtgo_matches WHERE replay = 0 AND my_list_id IS NOT NULL) AS matches_with_list",
        )
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
    opponent: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_cards: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    my_deck: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    my_concept: Option<String>,
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
    #[serde(default)]
    player: Option<String>,
}
fn two_hundred() -> usize {
    200
}

/// The matches a WHERE over mtgo_game_view selects (a match is included if any of its games
/// passes), newest first, with a compact per-game summary.
async fn matches_api(State(st): State<Shared>, headers: HeaderMap, Query(p): Query<MatchesParams>) -> Result<Json<Vec<MatchRow>>, ApiError> {
    let mut q = HashMap::new();
    if let Some(t) = &p.player {
        q.insert("player".to_string(), t.clone());
    }
    let name = auth(&st, &headers, &q)?;
    let dir = player_dir(&st, &name);
    let wher = p.where_sql.filter(|w| !w.trim().is_empty()).map(|w| format!("WHERE {w}")).unwrap_or_default();
    let limit = p.limit.min(2000);
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<MatchRow>, String> {
        use diesel::prelude::*;
        let conn = &mut dir.connect()?;
        mtgctl::mtgolog::db::ensure_schema(conn).map_err(|e| e.to_string())?;
        stats::ensure_view(conn).map_err(|e| e.to_string())?;
        diesel::sql_query(format!(
            "SELECT v.match_id, v.date, v.opponent, v.opp_cards, v.my_deck, v.my_concept, v.event_name, v.match_result, v.my_game_wins, v.opp_game_wins,
                    (SELECT GROUP_CONCAT(g.id || ':' || g.game_number || ':' || CASE g.on_play WHEN 1 THEN 'play' ELSE 'draw' END || ':' || g.my_mulligans || ':' || g.turns || ':' || COALESCE(g.winner, '?') || ':' || COALESCE(g.win_by, ''), '|')
                     FROM (SELECT * FROM mtgo_games WHERE match_id = v.match_id ORDER BY game_number) g) AS games
             FROM mtgo_game_view v
             {wher}
             GROUP BY v.match_id
             ORDER BY v.date DESC, v.match_id DESC
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
async fn game_detail(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<GameDetail>, ApiError> {
    let name = auth(&st, &headers, &q)?;
    let id: i32 = q.get("id").and_then(|s| s.parse().ok()).ok_or_else(|| bad("id required"))?;
    let dir = player_dir(&st, &name);
    let r = tokio::task::spawn_blocking(move || -> Result<GameDetail, String> {
        use diesel::prelude::*;
        let conn = &mut dir.connect()?;
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
    opponent: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_cards: Option<String>,
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
async fn games_api(State(st): State<Shared>, headers: HeaderMap, Query(p): Query<MatchesParams>) -> Result<Json<Vec<GameRow>>, ApiError> {
    let mut q = HashMap::new();
    if let Some(t) = &p.player {
        q.insert("player".to_string(), t.clone());
    }
    let name = auth(&st, &headers, &q)?;
    let dir = player_dir(&st, &name);
    let wher = p.where_sql.filter(|w| !w.trim().is_empty()).map(|w| format!("WHERE {w}")).unwrap_or_default();
    let limit = p.limit.min(5000);
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<GameRow>, String> {
        use diesel::prelude::*;
        let conn = &mut dir.connect()?;
        mtgctl::mtgolog::db::ensure_schema(conn).map_err(|e| e.to_string())?;
        stats::ensure_view(conn).map_err(|e| e.to_string())?;
        diesel::sql_query(format!(
            "SELECT game_id, match_id, date, opponent, opp_cards, my_deck, event_name, game_number, on_play, my_mulligans, opp_mulligans, turns, winner, win_by, minutes
             FROM mtgo_game_view {wher} ORDER BY date DESC, match_id DESC, game_number LIMIT {limit}"
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
        Cmd::CompactSessions => {
            if let Err(e) = compact_sessions(&cli.data) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Cmd::Serve { listen, web, repo } => {
            std::fs::create_dir_all(cli.data.join("players")).expect("data dir");
            let state = Arc::new(AppState { data: cli.data.clone(), reference: Reference::locate(&repo), sync_locks: Mutex::new(HashMap::new()) });
            let app = Router::new()
                .route("/api/me", get(me))
                .route("/api/players", get(players))
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
                .route("/api/lists", get(lists))
                .route("/api/list", get(list_detail))
                .fallback_service(ServeDir::new(&web))
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
