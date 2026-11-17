//! HTTP front-end over the mtgolog pipeline: players upload their MTGO log files, the server
//! pools every player's matches in one database and answers the same stats queries as `mtgctl mtgo stats`.
//! A request names the MTGO username it is about (`X-Player`, detected by `/api/detect` from the
//! logs and re-checked against the stored logs on every sync); who may upload, sync or mark as that
//! name is decided by the login or uploader key (see auth.rs).
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
use mtgctl::mtgolog::dataset::{self, Dataset};
use mtgctl::mtgolog::{filter, ldc, picker, stats};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tokio::sync::Mutex;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

#[path = "mtgctl-web/auth.rs"]
mod auth;
#[path = "mtgctl-web/mcp.rs"]
mod mcp;

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
    /// Eras from the timeline with their match counts, and a check that no banned card was played after its ban took effect. --all re-places every match.
    Eras {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        all: bool,
    },
    /// Download new Legacy Data Collection sheets and rebuild `<data>/ldc.db` from all of them. --all downloads every sheet again.
    LdcSync {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        all: bool,
    },
    /// Delete a player: their rows of the pooled database and their uploads.
    PlayerRemove { player: String },
    /// List the accounts, their MTGO names and uploader keys.
    Accounts,
    /// Link an MTGO name to an account (by id, Discord id or Discord name): its uploads become the account's.
    Bind { account: String, name: String },
    /// Unlink an MTGO name from its account.
    Unbind { name: String },
    /// Assign a player's registered lists to decks via the nearest reference decklist and print the decisions.
    Assign { player: String },
    /// Stamp a player's matches' format and place their registered lists and matches on the reference decks again (after references or card facts changed); a sync does the same for its player.
    Relabel { player: String },
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
        /// The Discord application and keyless names (deploy/auth.toml.example); holds the client secret.
        #[arg(long, default_value = "auth.toml")]
        auth: PathBuf,
    },
}


struct AppState {
    data: PathBuf,
    reference: Reference,
    /// one pipeline run at a time: SQLite has one writer
    sync_lock: Mutex<()>,
    auth: auth::Config,
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

/// Clients from before filters were entries send SQL as `where` or `split`: refuse them rather
/// than answer as if nothing were filtered.
async fn no_sql_params(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let q: HashMap<String, String> = Query::try_from_uri(req.uri()).map(|Query(q)| q).unwrap_or_default();
    if q.contains_key("where") || q.contains_key("split") {
        return bad("this server takes filters as JSON entries (`filters`, `groups`), not SQL (`where`, `split`): reload the page or update the client").into_response();
    }
    next.run(req).await
}

/// The Windows scripts are meant to be read in the browser before they are run: serve them as text.
async fn plain_text_scripts(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let ps1 = req.uri().path().ends_with(".ps1");
    let mut res = next.run(req).await;
    if ps1 && res.status().is_success() {
        res.headers_mut().insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("text/plain; charset=utf-8"));
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

/// A read connection that sees only the games of `names` (auth::scope): a TEMP view over the
/// dataset's view shadows it for every unqualified query on the connection. `query_only` then
/// keeps a fault in the SQL built from request filters from writing.
fn reader(ds: &Dataset, data: &Path, names: &[String]) -> Result<diesel::SqliteConnection, String> {
    use diesel::RunQueryDsl;
    let mut conn = ds.connect(data)?;
    let who = if names.is_empty() { "0".to_string() } else { format!("player IN ({})", names.iter().map(|n| sq(n)).collect::<Vec<_>>().join(", ")) };
    diesel::sql_query(format!("CREATE TEMP VIEW {v} AS SELECT * FROM main.{v} WHERE {who}", v = ds.view)).execute(&mut conn).map_err(|e| e.to_string())?;
    diesel::sql_query("PRAGMA query_only = 1").execute(&mut conn).map_err(|e| e.to_string())?;
    Ok(conn)
}

/// The WHERE and split conditions of a request's `filters` and `groups`, each a JSON array of
/// filter entries; see filter::query_sql.
fn query_sql(ds: &Dataset, filters: &str, except: Option<usize>, groups: &str, by: &[String]) -> Result<(Option<String>, Vec<String>), ApiError> {
    let parse = |k: &str, s: &str| -> Result<Vec<filter::Entry>, ApiError> {
        if s.trim().is_empty() {
            return Ok(vec![]);
        }
        serde_json::from_str(s).map_err(|e| bad(format!("{k}: {e}")))
    };
    filter::query_sql(ds, &parse("filters", filters)?, except, &parse("groups", groups)?, by).map_err(bad)
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

/// The players with uploads whose games the viewer sees (auth::scope).
async fn players(State(st): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<String>>, ApiError> {
    let mut out: Vec<String> = auth::scope(&st, &headers).await?.into_iter().filter(|n| player_dir(&st, n).logs().exists()).collect();
    out.sort();
    Ok(Json(out))
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
    let (name, _) = auth::player_access(&st, &headers, &q, false).await?;
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
    let (name, access) = auth::player_access(&st, &headers, &q, true).await?;
    let dir = player_dir(&st, &name);
    let mut used: u64 = stored_files(&dir).iter().map(|f| f.size).sum();
    let mut res = UploadResult { stored: vec![], skipped: vec![] };
    let (mut kind, mut fname, mut mtime, mut sha): (Option<String>, Option<String>, Option<u64>, Option<String>) = (None, None, None, None);
    // the client's UTC offset in minutes, JavaScript's getTimezoneOffset sign (UTC minus local; 420 for UTC-7)
    let mut tz: Option<i64> = None;
    let mut session_hashes: HashSet<String> = stored_files(&dir).into_iter().filter(|f| f.kind == "session").map(|f| f.sha256).collect();
    // a claim stores nothing until auth::claim has checked the whole upload
    let mut held: Option<(Vec<Part>, u64)> = matches!(access, auth::Access::Claim(_)).then(|| (vec![], 0));
    if held.is_none() {
        dir.ensure().map_err(|e| internal(e.to_string()))?;
    }
    while let Some(field) = mp.next_field().await.map_err(|e| bad(e.to_string()))? {
        match field.name().unwrap_or("") {
            "kind" => kind = Some(field.text().await.map_err(|e| bad(e.to_string()))?),
            "name" => fname = Some(field.text().await.map_err(|e| bad(e.to_string()))?),
            "mtime" => mtime = field.text().await.map_err(|e| bad(e.to_string()))?.trim().parse().ok(),
            "sha256" => sha = Some(field.text().await.map_err(|e| bad(e.to_string()))?.trim().to_lowercase()),
            "tz" => tz = field.text().await.map_err(|e| bad(e.to_string()))?.trim().parse().ok(),
            "file" => {
                let bytes: Bytes = field.bytes().await.map_err(|e| bad(e.to_string()))?;
                let part = Part { kind: kind.take().ok_or_else(|| bad("kind before file"))?, name: fname.take().ok_or_else(|| bad("name before file"))?, mtime: mtime.take().unwrap_or(0), tz, sha: sha.take(), bytes };
                match &mut held {
                    Some((parts, size)) => {
                        *size += part.bytes.len() as u64;
                        if *size > MAX_CLAIM_BYTES {
                            return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, format!("the first upload as {name} is limited to {} MB", MAX_CLAIM_BYTES >> 20)));
                        }
                        parts.push(part);
                    }
                    None => put(&dir, &name, part, &mut used, &mut session_hashes, &mut res)?,
                }
            }
            other => return Err(bad(format!("unexpected field {other}"))),
        }
    }
    if let (auth::Access::Claim(account), Some((parts, _))) = (access, held) {
        let logs: Vec<&[u8]> = parts.iter().filter(|p| p.kind == "game").map(|p| &p.bytes[..]).collect();
        auth::claim(&st.data, account, &name, &logs).map_err(|e| ApiError(StatusCode::FORBIDDEN, e))?;
        dir.ensure().map_err(|e| internal(e.to_string()))?;
        for part in parts {
            put(&dir, &name, part, &mut used, &mut session_hashes, &mut res)?;
        }
    }
    Ok(Json(res))
}

/// A claiming upload is held in memory whole (see upload); the uploaders send at most 48 MB at once.
const MAX_CLAIM_BYTES: u64 = 64 * 1024 * 1024;

/// One uploaded file with the fields sent before it.
struct Part {
    kind: String,
    name: String,
    mtime: u64,
    tz: Option<i64>,
    sha: Option<String>,
    bytes: Bytes,
}

fn put(dir: &PlayerDir, player: &str, p: Part, used: &mut u64, session_hashes: &mut HashSet<String>, res: &mut UploadResult) -> Result<(), ApiError> {
    if *used + p.bytes.len() as u64 > MAX_PLAYER_BYTES {
        return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, format!("{player} is over the {} MB storage cap", MAX_PLAYER_BYTES >> 20)));
    }
    match store_file(dir, &p.kind, &p.name, p.mtime, p.tz, p.sha.as_deref(), &p.bytes, session_hashes) {
        Ok(Some(stored)) => {
            *used += p.bytes.len() as u64;
            res.stored.push(stored)
        }
        Ok(None) => res.skipped.push(p.name),
        Err(e) => return Err(bad(format!("{}: {e}", p.name))),
    }
    Ok(())
}

fn store_file(dir: &PlayerDir, kind: &str, name: &str, mtime_ms: u64, tz_min: Option<i64>, raw_sha: Option<&str>, bytes: &[u8], session_hashes: &mut HashSet<String>) -> Result<Option<String>, String> {
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
    let s = mtgctl::mtgolog::eras::stamp(conn, &reference.timeline, true)?;
    println!("eras: {} matches re-placed in {} eras", s.placed, s.eras);
    println!("marks: {} by hand applied", mtgctl::mtgolog::marks::apply(conn, data, None)?);
    Ok(())
}

fn eras_report(data: &Path, timeline: &Path, all: bool) -> Result<(), String> {
    use diesel::prelude::*;
    use diesel::sql_types::{BigInt, Text};
    use mtgctl::mtgolog::eras;
    let conn = &mut pipeline::connect(data)?;
    mtgctl::mtgolog::db::ensure_schema(conn).map_err(|e| e.to_string())?;
    let s = eras::stamp(conn, timeline, all)?;
    println!("{} matches placed{}", s.placed, s.from.map(|d| format!(" (re-placed from {} on)", if d.is_empty() { "the start" } else { &d })).unwrap_or_default());
    #[derive(diesel::QueryableByName)]
    struct R {
        #[diesel(sql_type = Text)]
        start_date: String,
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = Text)]
        banned: String,
        #[diesel(sql_type = BigInt)]
        matches: i64,
    }
    let rows: Vec<R> = diesel::sql_query("SELECT e.start_date, e.name, e.banned || CASE WHEN e.unbanned <> '' THEN ' / unbanned ' || e.unbanned ELSE '' END AS banned, (SELECT COUNT(*) FROM mtgo_matches m WHERE m.era_id = e.id AND m.replay = 0) AS matches FROM mtgo_eras e ORDER BY e.start_date").load(conn).map_err(|e| e.to_string())?;
    let before: i64 = diesel::sql_query("SELECT COUNT(*) AS n FROM mtgo_matches WHERE era_id IS NULL AND replay = 0").get_result::<mtgctl::mtgolog::db::CountRow>(conn).map_err(|e| e.to_string())?.n;
    println!("{:<11} {:>7}  {:<42} banned", "from", "matches", "era");
    for r in &rows {
        println!("{:<11} {:>7}  {:<42} {}", r.start_date, r.matches, r.name, r.banned);
    }
    if before > 0 {
        println!("{before} matches predate the first era");
    }
    let players: Vec<String> = mtgctl::mtgolog::db::mtgo_matches::table.select(mtgctl::mtgolog::db::mtgo_matches::me).distinct().load(conn).map_err(|e| e.to_string())?;
    for p in players {
        if let Some((days, last)) = eras::staleness(conn, timeline, &p)? {
            println!("{p}: newest match is {days} days past the timeline's last event ({last}); the timeline is probably missing a set release or B&R change");
        }
    }
    println!("\nbanned cards, games played before / on or after the ban in Legacy sessions / on or after elsewhere (another format, or no session log):");
    for (date, card, before, after, elsewhere) in eras::ban_check(conn, timeline)? {
        println!("  {date} {card:<32} {before:>5} / {after:<5} / {elsewhere:<5}{}", if after > 0 { "  <-- after the ban in Legacy: check the effective date" } else { "" });
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
    let (name, access) = auth::player_access(&st, &headers, &q, true).await?;
    if let auth::Access::Claim(_) = access {
        return Err(bad(format!("nothing uploaded as {name} yet")));
    }
    Ok(Json(run_sync(st, name).await?))
}

#[derive(Deserialize)]
struct StatsParams {
    #[serde(default)]
    by: String,
    /// JSON filter entries; see query_sql
    #[serde(default)]
    filters: String,
    /// JSON group entries: a card sentence among them is a yes/no split after the `by` columns
    #[serde(default)]
    groups: String,
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
    game: Option<bool>,
    #[serde(default)]
    ds: Option<String>,
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

/// The dataset a request names (`ds`), the uploaded logs when it names none.
fn dataset(key: Option<&str>) -> Result<&'static Dataset, ApiError> {
    let key = key.filter(|k| !k.is_empty()).unwrap_or("mtgo");
    dataset::by_key(key).ok_or_else(|| bad(format!("no dataset {key}")))
}

async fn stats_api(State(st): State<Shared>, headers: HeaderMap, Query(p): Query<StatsParams>) -> Result<Json<stats::StatsResult>, ApiError> {
    let ds = dataset(p.ds.as_deref())?;
    let names = auth::scope(&st, &headers).await?;
    let by = split_cols(&p.by);
    let (where_sql, split) = query_sql(ds, &p.filters, None, &p.groups, &by)?;
    let query = stats::StatsQuery { by, split, game: p.game, per: None, where_sql, min_games: p.min, sort: p.sort, limit: p.limit, full: p.full, hist: split_cols(&p.hist) };
    let r = tokio::task::spawn_blocking(move || {
        let conn = &mut reader(ds, &st.data, &names)?;
        stats::compute(conn, ds, &query)
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(r))
}

async fn columns(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Result<Json<Vec<String>>, ApiError> {
    let ds = dataset(q.get("ds").map(String::as_str))?;
    let cols = tokio::task::spawn_blocking(move || {
        let conn = &mut ds.connect(&st.data)?;
        picker::view_columns(conn, ds).map_err(|e| e.to_string())
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

#[derive(Deserialize)]
struct ExperimentalReq {
    id: i32,
    on: bool,
}

/// Tag one of the viewer's registered lists as an experimental brew, or clear the tag.
async fn list_experimental(State(st): State<Shared>, headers: HeaderMap, Json(req): Json<ExperimentalReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let names = own_names(&st, &headers).await?;
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        use diesel::prelude::*;
        let conn = &mut pool(&st).map_err(internal)?;
        #[derive(diesel::QueryableByName)]
        struct H { #[diesel(sql_type = diesel::sql_types::Text)] me: String, #[diesel(sql_type = diesel::sql_types::Text)] hash: String }
        let h: H = diesel::sql_query(format!("SELECT me, hash FROM mtgo_lists WHERE id = {}", req.id)).get_result(conn).map_err(|_| bad(format!("no list {}", req.id)))?;
        if !names.iter().any(|n| n.eq_ignore_ascii_case(&h.me)) {
            return Err(ApiError(StatusCode::FORBIDDEN, "you can tag only your own lists".into()));
        }
        mtgctl::mtgolog::marks::record(conn, &st.data, &mtgctl::mtgolog::marks::Mark { at: mtgctl::mtgolog::marks::now(), by: h.me.clone(), me: h.me, guid: None, hash: Some(h.hash), field: "experimental".into(), value: Some(if req.on { "1" } else { "0" }.into()), extra: None }).map_err(bad)
    })
    .await
    .map_err(|e| internal(e.to_string()))??;
    Ok(Json(serde_json::json!({ "id": req.id, "experimental": req.on })))
}

#[derive(Deserialize)]
struct MarkReq {
    match_id: i32,
    /// see marks::MATCH_FIELDS
    field: String,
    /// empty or absent clears the field; a deck must be one of the taxonomy's, exclude is "1"
    value: Option<String>,
}

/// The MTGO names of the viewer's account; a request without a login (or key) is refused.
async fn own_names(st: &Shared, headers: &HeaderMap) -> Result<Vec<String>, ApiError> {
    let v = auth::viewer(st, headers, true).await?;
    if !v.can_write() {
        return Err(ApiError(StatusCode::FORBIDDEN, "a read key only reads".into()));
    }
    Ok(v.account.ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "log in first (the link at the top of the page)".into()))?.names)
}

/// A hand mark on one of the viewer's own matches (marks.rs).
async fn mark_api(State(st): State<Shared>, headers: HeaderMap, Json(req): Json<MarkReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let names = own_names(&st, &headers).await?;
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        use diesel::prelude::*;
        let conn = &mut pool(&st)?;
        #[derive(diesel::QueryableByName)]
        struct K { #[diesel(sql_type = diesel::sql_types::Text)] me: String, #[diesel(sql_type = diesel::sql_types::Text)] guid: String }
        let k: K = diesel::sql_query(format!("SELECT me, guid FROM mtgo_matches WHERE id = {}", req.match_id)).get_result(conn).map_err(|_| format!("no match {}", req.match_id))?;
        if !names.iter().any(|n| n.eq_ignore_ascii_case(&k.me)) {
            return Err("you can mark only your own matches".into());
        }
        let by = k.me.clone();
        let mut value = req.value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let mut extra = None;
        match (req.field.as_str(), &value) {
            ("opp_deck" | "my_deck", Some(deck)) => {
                #[derive(diesel::QueryableByName)]
                struct A { #[diesel(sql_type = diesel::sql_types::Text)] name: String }
                let a: Option<A> = diesel::sql_query(format!("SELECT a.name FROM decks d JOIN archetypes a ON a.id = d.archetype_id WHERE d.name = '{}'", deck.replace('\'', "''"))).get_result(&mut shared_db(&st.data)?).optional().map_err(|e| e.to_string())?;
                extra = Some(a.ok_or_else(|| format!("{deck} is not a deck of the taxonomy"))?.name);
            }
            ("exclude", Some(_)) => value = Some("1".into()),
            _ => {}
        }
        mtgctl::mtgolog::marks::record(conn, &st.data, &mtgctl::mtgolog::marks::Mark { at: mtgctl::mtgolog::marks::now(), by, me: k.me, guid: Some(k.guid), hash: None, field: req.field, value, extra })
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// The taxonomy's decks with their archetypes, for the mark editor.
async fn decks_api(State(st): State<Shared>) -> Result<Json<serde_json::Value>, ApiError> {
    let data = st.data.clone();
    let v = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        use diesel::prelude::*;
        #[derive(diesel::QueryableByName, Serialize)]
        struct D { #[diesel(sql_type = diesel::sql_types::Text)] archetype: String, #[diesel(sql_type = diesel::sql_types::Text)] deck: String }
        let rows: Vec<D> = diesel::sql_query("SELECT a.name AS archetype, d.name AS deck FROM decks d JOIN archetypes a ON a.id = d.archetype_id ORDER BY 1, 2").load(&mut shared_db(&data)?).map_err(|e| e.to_string())?;
        Ok(serde_json::to_value(rows).unwrap())
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    Ok(Json(v))
}

/// Every card the viewer's game logs name or registered lists hold, for the card filters' suggestions.
async fn cards_seen(State(st): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<String>>, ApiError> {
    let names = auth::scope(&st, &headers).await?;
    let rows = tokio::task::spawn_blocking(move || {
        use diesel::prelude::*;
        let conn = &mut reader(&dataset::MTGO, &st.data, &names)?;
        #[derive(diesel::QueryableByName)]
        struct R {
            #[diesel(sql_type = diesel::sql_types::Text)]
            name: String,
        }
        let rows: Vec<R> = diesel::sql_query("SELECT card AS name FROM mtgo_card_events WHERE game_id IN (SELECT game_id FROM mtgo_game_view) UNION SELECT name FROM mtgo_list_cards WHERE list_id IN (SELECT my_list_id FROM mtgo_game_view) ORDER BY name").load(conn).map_err(|e| e.to_string())?;
        Ok::<_, String>(rows.into_iter().map(|r| r.name).collect::<Vec<_>>())
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    Ok(Json(rows))
}

/// Distinct values of one view column under `filters` but the one at `except` (the filter being
/// edited), most matches first.
async fn values(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Vec<ValueRow>>, ApiError> {
    let col = q.get("col").cloned().ok_or_else(|| bad("col required"))?;
    if !col.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(bad("bad column"));
    }
    let ds = dataset(q.get("ds").map(String::as_str))?;
    let except = match q.get("except").filter(|x| !x.is_empty()) {
        Some(x) => Some(x.parse::<usize>().map_err(|_| bad("except: not an index"))?),
        None => None,
    };
    let arg = |k: &str| q.get(k).map_or("", String::as_str);
    let wher: Vec<String> = query_sql(ds, arg("filters"), except, arg("groups"), &[])?.0.into_iter().collect();
    let names = auth::scope(&st, &headers).await?;
    let rows = tokio::task::spawn_blocking(move || {
        let conn = &mut reader(ds, &st.data, &names)?;
        picker::column_values(conn, ds, &col, &wher).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(rows.into_iter().map(|(value, matches, games)| ValueRow { value, matches, games }).collect()))
}

#[derive(Serialize)]
struct Schema {
    datasets: Vec<(&'static str, &'static str)>,
    dataset: &'static Dataset,
    /// the catalog's columns the view really has
    present: Vec<String>,
    defaults: serde_json::Value,
}

/// What the stats page builds itself from: the dataset's column catalog, features and default query.
async fn schema(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Result<Json<Schema>, ApiError> {
    let ds = dataset(q.get("ds").map(String::as_str))?;
    let present = tokio::task::spawn_blocking(move || {
        let conn = &mut ds.connect(&st.data)?;
        picker::view_columns(conn, ds).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    let datasets = dataset::SERVED.iter().map(|d| (d.key, d.title)).collect();
    Ok(Json(Schema { datasets, dataset: ds, present, defaults: ds.defaults() }))
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
    let name = auth::own_name(&st, &headers, &q).await?;
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
    let name = auth::own_name(&st, &headers, &q).await?;
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
    let (name, _) = auth::player_access(&st, &headers, &q, false).await?;
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
    player: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    format: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    format_source: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    legacy: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    note: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    excluded: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_deck_source: Option<String>,
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
    /// JSON filter entries; see query_sql
    #[serde(default)]
    filters: String,
    /// JSON group entries: only what they need applies
    #[serde(default)]
    groups: String,
    #[serde(default = "two_hundred")]
    limit: usize,
}
fn two_hundred() -> usize {
    200
}

/// The matches the filters select over mtgo_game_view (a match is included if any of its games
/// passes), newest first, with a compact per-game summary.
async fn matches_api(State(st): State<Shared>, headers: HeaderMap, Query(p): Query<MatchesParams>) -> Result<Json<Vec<MatchRow>>, ApiError> {
    let ds = dataset(None)?;
    let names = auth::scope(&st, &headers).await?;
    let wher = query_sql(ds, &p.filters, None, &p.groups, &[])?.0.map(|w| format!("WHERE {w}")).unwrap_or_default();
    let limit = p.limit.min(2000);
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<MatchRow>, String> {
        use diesel::prelude::*;
        let conn = &mut reader(ds, &st.data, &names)?;
        diesel::sql_query(format!(
            "SELECT v.match_id, v.player, v.format, v.format_source, v.legacy, v.note, v.excluded, v.opp_deck_source, v.date, v.match_start AS start, v.opponent, v.my_list, v.my_deck, v.my_archetype, v.my_deck_source, v.opp_deck, v.opp_archetype, v.opp_deck_candidates, v.opp_candidate_posteriors, v.opp_posterior, v.opp_surprise, v.opp_archetype_surprise, v.opp_misfit, v.opp_seen, v.opp_colors, v.event_name, v.match_result, v.my_game_wins, v.opp_game_wins,
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
    #[diesel(sql_type = diesel::sql_types::Text)]
    #[serde(skip)]
    ts: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    turn: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    side: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Text)]
    kind: String,
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
    /// my hand, draws, sideboarding and pile were all found in my session log (see hands::derive)
    hand_known: bool,
    /// my card events only the session log shows, in log order
    my_cards: Vec<MyCardEvent>,
    /// my 60 after sideboarding, when hand_known
    my_deck: Vec<DeckCard>,
    /// my hand after each change, when asked for (`hands=1`) and the session log was uploaded
    #[serde(skip_serializing_if = "Option::is_none")]
    my_hand: Option<Vec<HandState>>,
}

#[derive(diesel::QueryableByName, Serialize)]
struct MyCardEvent {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    seq: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    turn: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    verb: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    card: String,
}

#[derive(diesel::QueryableByName, Serialize)]
struct DeckCard {
    #[diesel(sql_type = diesel::sql_types::Text)]
    card: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

#[derive(Serialize)]
struct HandState {
    /// the last log line at or before the state's second
    seq: i32,
    turn: i32,
    cards: Vec<String>,
}

#[derive(diesel::QueryableByName)]
struct Flag {
    #[diesel(sql_type = diesel::sql_types::Bool)]
    v: bool,
}

/// Session-log card verbs: mine only, and true only for games with `my_hand_known` (`pile`:
/// `my_pile_known`); see filter::VERBS.
fn hidden_verbs() -> impl Iterator<Item = &'static str> {
    mtgctl::mtgolog::filter::VERBS.iter().filter(|(_, _, needs)| needs.is_some()).map(|(v, _, _)| *v)
}

/// Per game of `sel` (columns game_id, game_list_id, my_hand_known) whose hand is known, my main
/// deck after sideboarding as (game_id, card, n): the registered list's main deck plus what the
/// snapshots show boarded in, less what they show boarded out.
fn deck_sql(sel: &str) -> String {
    format!(
        "SELECT d.game_id, d.card, SUM(d.n) AS n FROM (
           SELECT s.game_id, lc.name AS card, lc.qty AS n FROM {sel} s JOIN mtgo_list_cards lc ON lc.list_id = s.game_list_id AND lc.sideboard = 0 WHERE s.my_hand_known = 1
           UNION ALL
           SELECT c.game_id, c.card, CASE c.verb WHEN 'boarded_in' THEN 1 ELSE -1 END FROM mtgo_card_events c JOIN {sel} s ON s.game_id = c.game_id
            WHERE s.my_hand_known = 1 AND c.side = 'me' AND c.verb IN ('boarded_in', 'boarded_out')
         ) d GROUP BY d.game_id, d.card HAVING SUM(d.n) > 0"
    )
}

/// A read connection for game `id`, which must be one of `names`' games: these handlers read the
/// game's tables by id, past the scoped view.
fn visible_game(st: &AppState, names: &[String], id: i32) -> Result<diesel::SqliteConnection, String> {
    use diesel::prelude::*;
    let mut conn = reader(&dataset::MTGO, &st.data, names)?;
    let n = diesel::sql_query(format!("SELECT COUNT(*) AS n FROM mtgo_game_view WHERE game_id = {id}")).get_result::<mtgctl::mtgolog::db::CountRow>(&mut conn).map_err(|e| e.to_string())?.n;
    if n == 0 {
        return Err(format!("no game {id}"));
    }
    Ok(conn)
}

/// MTGO's card markup `@[Name@:catalog,instance:@]`.
static CARD_REF: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"@\[([^@]*)@:([0-9]*),?([0-9]*):@\]").unwrap());

/// Strip MTGO's card markup down to the name.
fn plain_text(s: &str) -> String {
    CARD_REF.replace_all(s, "$1").into_owned()
}

/// One game: its facts, the cards each side played (with first turn), the event log, and my
/// session-log facts (`hands=1` adds my hand after each change, read from the stored session log).
async fn game_detail(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<GameDetail>, ApiError> {
    let id: i32 = q.get("id").and_then(|s| s.parse().ok()).ok_or_else(|| bad("id required"))?;
    let hands = q.get("hands").is_some_and(|v| v == "1");
    let names = auth::scope(&st, &headers).await?;
    let r = tokio::task::spawn_blocking(move || -> Result<GameDetail, String> {
        use diesel::prelude::*;
        let conn = &mut visible_game(&st, &names, id)?;
        let game: GameMeta = diesel::sql_query(format!("SELECT g.id, g.match_id, g.game_number, g.on_play, g.turns, g.winner, g.win_by, g.my_mulligans, g.opp_mulligans, m.opponent, g.start_ts FROM mtgo_games g JOIN mtgo_matches m ON m.id = g.match_id WHERE g.id = {id}")).get_result(conn).map_err(|e| format!("no game {id}: {e}"))?;
        let cards: Vec<PlayedCard> = diesel::sql_query(format!("SELECT side, card AS name, COUNT(*) AS plays, MIN(turn) AS first_turn FROM mtgo_card_events WHERE game_id = {id} AND verb IN ('cast', 'land') GROUP BY side, card ORDER BY side, first_turn, name")).load(conn).map_err(|e| e.to_string())?;
        let mut events: Vec<EventRow> = diesel::sql_query(format!("SELECT seq, ts, turn, side, kind, text FROM mtgo_events WHERE game_id = {id} ORDER BY seq")).load(conn).map_err(|e| e.to_string())?;
        for e in &mut events {
            e.text = plain_text(&e.text);
        }
        let hand_known = diesel::sql_query(format!("SELECT COALESCE(hand_known, 0) AS v FROM mtgo_games WHERE id = {id}")).get_result::<Flag>(conn).map_err(|e| e.to_string())?.v;
        // in_hand is left out: my_hand shows the hand itself
        let verbs = hidden_verbs().filter(|v| *v != "in_hand").map(|v| format!("'{v}'")).collect::<Vec<_>>().join(", ");
        let my_cards: Vec<MyCardEvent> = diesel::sql_query(format!("SELECT seq, turn, verb, card FROM mtgo_card_events WHERE game_id = {id} AND side = 'me' AND verb IN ({verbs}) ORDER BY seq, id")).load(conn).map_err(|e| e.to_string())?;
        let sel = format!("(SELECT id AS game_id, list_id AS game_list_id, CASE WHEN hand_known THEN 1 ELSE 0 END AS my_hand_known FROM mtgo_games WHERE id = {id})");
        let my_deck: Vec<DeckCard> = diesel::sql_query(format!("SELECT card, n FROM ({}) ORDER BY card", deck_sql(&sel))).load(conn).map_err(|e| e.to_string())?;
        let my_hand = if hands {
            let gs = game_states(&st, conn, id, &game.start_ts)?;
            // states outside the game's own time are from a replay (see hands::derive)
            let (first, last) = (events.first().map(|e| &e.ts[..19.min(e.ts.len())]), events.last().map(|e| &e.ts[..19.min(e.ts.len())]));
            let mut out: Vec<HandState> = Vec::new();
            for s in &gs.snaps {
                let at = s.at.format("%Y-%m-%d %H:%M:%S").to_string();
                if first.is_some_and(|f| at.as_str() < f) || last.is_some_and(|l| at.as_str() > l) {
                    continue;
                }
                let me = s.players.iter().find(|p| p.name == gs.me).map(|p| p.id);
                let mut hand: Vec<String> = s.cards.iter().filter(|c| c.zone == "Hand" && Some(c.owner) == me).map(|c| gs.names.get(&c.catalog_id).cloned().unwrap_or_else(|| mtgctl::mtgolog::decklist::placeholder_name(c.catalog_id))).collect();
                hand.sort();
                // before the first deal the hand is empty
                if out.last().map_or(hand.is_empty(), |h| h.cards == hand) {
                    continue;
                }
                let e = events.iter().rev().find(|e| &e.ts[..19.min(e.ts.len())] <= at.as_str());
                out.push(HandState { seq: e.map_or(0, |e| e.seq), turn: e.map_or(0, |e| e.turn), cards: hand });
            }
            Some(out)
        } else {
            None
        };
        Ok(GameDetail { game, cards, events, hand_known, my_cards, my_deck, my_hand })
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(r))
}

#[derive(diesel::QueryableByName)]
struct ReplayEventRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    seq: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    ts: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    turn: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    side: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Text)]
    kind: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    text: String,
}

#[derive(Serialize)]
struct ReplayEvent {
    seq: i32,
    ts: String,
    turn: i32,
    side: Option<String>,
    kind: String,
    text: String,
    /// the cards the line names, in order
    cards: Vec<String>,
}

#[derive(diesel::QueryableByName)]
struct ReplayKey {
    #[diesel(sql_type = diesel::sql_types::Text)]
    me: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    mtgo_game_id: Option<i64>,
}

#[derive(Serialize)]
struct ReplayPlayer {
    side: &'static str,
    name: String,
    life: i32,
    hand: i32,
    library: i32,
}

#[derive(Serialize)]
struct ReplayCard {
    id: i64,
    name: String,
    zone: String,
    /// owner's side
    side: &'static str,
    controller: &'static str,
    land: bool,
}

#[derive(Serialize)]
struct ReplayState {
    at: String,
    players: Vec<ReplayPlayer>,
    cards: Vec<ReplayCard>,
}

#[derive(Serialize)]
struct Replay {
    game: GameMeta,
    events: Vec<ReplayEvent>,
    states: Vec<ReplayState>,
    /// why `states` is empty
    note: Option<String>,
}

/// One game's state snapshots from its player's stored session logs (see `mtgolog::snapshot`),
/// with a name for every catalog id they show; `note` says why `snaps` is empty.
struct GameStates {
    me: String,
    snaps: Vec<mtgctl::mtgolog::snapshot::Snapshot>,
    names: HashMap<i64, String>,
    note: Option<String>,
}

fn game_states(st: &AppState, conn: &mut diesel::SqliteConnection, id: i32, start_ts: &str) -> Result<GameStates, String> {
    use diesel::prelude::*;
    use mtgctl::mtgolog::{decklist, snapshot};
    let key: ReplayKey = diesel::sql_query(format!("SELECT m.me, (SELECT d.mtgo_game_id FROM mtgo_decklists d WHERE d.game_db_id = g.id) AS mtgo_game_id FROM mtgo_games g JOIN mtgo_matches m ON m.id = g.match_id WHERE g.id = {id}")).get_result(conn).map_err(|e| e.to_string())?;
    let Some(mtgo_id) = key.mtgo_game_id else {
        return Ok(GameStates { me: key.me, snaps: Vec::new(), names: HashMap::new(), note: Some("no session log names this game (the client log of that session was not uploaded)".into()) });
    };
    let day = chrono::NaiveDate::parse_from_str(&start_ts[..10], "%Y-%m-%d").map_err(|e| e.to_string())?;
    let cfg = player_dir(st, &key.me).config(&key.me);
    let snaps = snapshot::game_snapshots(&cfg, mtgo_id, day);
    let ids: std::collections::BTreeSet<i64> = snaps.iter().flat_map(|s| s.cards.iter().map(|c| c.catalog_id)).collect();
    // resolving unknown ids writes the name cache: only while no sync is writing
    let names = match st.sync_lock.try_lock() {
        Ok(_g) => decklist::resolve_names(conn, &cfg, &ids, true),
        Err(_) => decklist::known_names(conn),
    }
    .map_err(|e| e.to_string())?;
    let note = snaps.is_empty().then(|| format!("no snapshots of MTGO game {mtgo_id} in the stored session logs"));
    Ok(GameStates { me: key.me, snaps, names, note })
}

/// One game's event log plus its state snapshots from the player's stored session logs (see
/// `mtgolog::snapshot`), card names resolved; sideboards are left out.
async fn replay_api(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Replay>, ApiError> {
    let id: i32 = q.get("id").and_then(|s| s.parse().ok()).ok_or_else(|| bad("id required"))?;
    let names = auth::scope(&st, &headers).await?;
    let r = tokio::task::spawn_blocking(move || -> Result<Replay, String> {
        use diesel::prelude::*;
        use mtgctl::mtgolog::snapshot;
        let conn = &mut visible_game(&st, &names, id)?;
        let game: GameMeta = diesel::sql_query(format!("SELECT g.id, g.match_id, g.game_number, g.on_play, g.turns, g.winner, g.win_by, g.my_mulligans, g.opp_mulligans, m.opponent, g.start_ts FROM mtgo_games g JOIN mtgo_matches m ON m.id = g.match_id WHERE g.id = {id}")).get_result(conn).map_err(|e| format!("no game {id}: {e}"))?;
        let rows: Vec<ReplayEventRow> = diesel::sql_query(format!("SELECT seq, ts, turn, side, kind, text FROM mtgo_events WHERE game_id = {id} ORDER BY seq")).load(conn).map_err(|e| e.to_string())?;
        // (time, object id, name) for every card the log mentions: snapshots share the log's object
        // ids but may use another printing's catalog id, which Scryfall need not know (back faces)
        let mut logged: Vec<(String, i64, String)> = Vec::new();
        let mut events: Vec<ReplayEvent> = Vec::with_capacity(rows.len());
        for e in rows {
            let mut cards = Vec::new();
            for c in CARD_REF.captures_iter(&e.text) {
                if let Ok(obj) = c[3].parse() {
                    logged.push((e.ts.clone(), obj, c[1].to_string()));
                }
                cards.push(c[1].to_string());
            }
            events.push(ReplayEvent { seq: e.seq, ts: e.ts, turn: e.turn, side: e.side, kind: e.kind, text: plain_text(&e.text), cards });
        }
        let GameStates { me, snaps, names, note } = game_states(&st, conn, id, &game.start_ts)?;
        if snaps.is_empty() {
            return Ok(Replay { game, events, states: Vec::new(), note });
        }
        let lands = shared_db(&st.data).and_then(|mut sh| mtgctl::mtgolog::reference::load_lands(&mut sh).map_err(|e| e.to_string()))?;
        let states = snaps
            .into_iter()
            .map(|s| {
                let side = |pid: i64| if s.players.iter().any(|p| p.id == pid && p.name == me) { "me" } else { "opp" };
                let at = s.at.format("%Y-%m-%d %H:%M:%S").to_string();
                // the object's name as of this state (a transform keeps the id), else its first
                let by_object = |obj: i64| {
                    let named = logged.iter().filter(|l| l.1 == obj);
                    let first = named.clone().next();
                    named.filter(|l| l.0 <= at).last().or(first).map(|l| l.2.clone())
                };
                let name = |c: &snapshot::SnapCard| names.get(&c.catalog_id).cloned().or_else(|| by_object(c.id)).unwrap_or_else(|| format!("#{}", c.catalog_id));
                ReplayState {
                    at: at.clone(),
                    players: s.players.iter().map(|p| ReplayPlayer { side: side(p.id), name: p.name.clone(), life: p.life, hand: p.hand, library: p.library }).collect(),
                    cards: s
                        .cards
                        .iter()
                        .filter(|c| !matches!(c.zone.as_str(), "Sideboard" | "Nowhere" | "Effects"))
                        .map(|c| {
                            let name = name(c);
                            ReplayCard { id: c.id, land: lands.contains(&name), name, zone: c.zone.clone(), side: side(c.owner), controller: side(c.controller) }
                        })
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        Ok(Replay { game, events, states, note })
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
    player: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    format: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    format_source: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    legacy: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    note: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    excluded: i32,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    opp_deck_source: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    my_deck_source: Option<String>,
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

/// One row per game the filters select, newest first.
async fn games_api(State(st): State<Shared>, headers: HeaderMap, Query(p): Query<MatchesParams>) -> Result<Json<Vec<GameRow>>, ApiError> {
    let ds = dataset(None)?;
    let names = auth::scope(&st, &headers).await?;
    let wher = query_sql(ds, &p.filters, None, &p.groups, &[])?.0.map(|w| format!("WHERE {w}")).unwrap_or_default();
    let limit = p.limit.min(5000);
    let rows = tokio::task::spawn_blocking(move || -> Result<Vec<GameRow>, String> {
        use diesel::prelude::*;
        let conn = &mut reader(ds, &st.data, &names)?;
        diesel::sql_query(format!(
            "SELECT game_id, match_id, player, format, format_source, legacy, note, excluded, opp_deck_source, my_deck_source, date, game_start AS start, opponent, opp_deck, opp_archetype, opp_deck_candidates, opp_candidate_posteriors, opp_posterior, opp_surprise, opp_archetype_surprise, opp_misfit, opp_seen, opp_colors, my_list, my_deck, event_name, game_number, on_play, my_mulligans, opp_mulligans, turns, winner, win_by, minutes
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

#[derive(Deserialize)]
struct CardCountParams {
    /// JSON filter entries; see query_sql
    #[serde(default)]
    filters: String,
    /// JSON group entries: only what they need applies
    #[serde(default)]
    groups: String,
    /// comma-separated filter::VERBS names and `deck`; none = all
    #[serde(default)]
    verbs: Option<String>,
    /// a JSON array of card names for `rows`; none = every card
    #[serde(default)]
    cards: Option<String>,
    /// me | opp; none = both
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    by_game: bool,
    #[serde(default = "five_hundred")]
    limit: usize,
}
fn five_hundred() -> usize {
    500
}

#[derive(diesel::QueryableByName)]
struct CardCountRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    game_id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    side: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    verb: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    card: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

#[derive(Serialize)]
struct CardCount {
    #[serde(skip_serializing_if = "Option::is_none")]
    game_id: Option<i32>,
    side: String,
    verb: String,
    /// absent in `totals`, which sum every card
    #[serde(skip_serializing_if = "Option::is_none")]
    card: Option<String>,
    n: i64,
    /// games with at least one; absent per game
    #[serde(skip_serializing_if = "Option::is_none")]
    games: Option<i64>,
}

#[derive(Serialize)]
struct CardCounts {
    games: i64,
    /// of `games`, those whose session-log verbs and `deck` are counted
    games_hand_known: i64,
    rows: Vec<CardCount>,
    totals: Vec<CardCount>,
    /// `rows` stopped at the limit
    truncated: bool,
}

#[derive(diesel::QueryableByName)]
struct SelCount {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    games: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    hand: i64,
}

/// Card events counted over the selected games, per (side, verb, card) or per game; `deck` is my
/// main deck after sideboarding (see deck_sql). Session-log verbs count only the games whose
/// facts are known, so a missing count means none happened.
async fn card_counts(State(st): State<Shared>, headers: HeaderMap, Query(p): Query<CardCountParams>) -> Result<Json<CardCounts>, ApiError> {
    use mtgctl::mtgolog::filter::{sq, VERBS};
    let ds = dataset(None)?;
    let names = auth::scope(&st, &headers).await?;
    let wher = query_sql(ds, &p.filters, None, &p.groups, &[])?.0.map(|w| format!("WHERE {w}")).unwrap_or_default();
    let verbs: Vec<String> = p.verbs.iter().flat_map(|v| v.split(',')).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
    if let Some(v) = verbs.iter().find(|v| v.as_str() != "deck" && !VERBS.iter().any(|(k, _, _)| k == v)) {
        return Err(bad(format!("unknown verb {v:?}")));
    }
    let side = p.side.filter(|s| !s.is_empty());
    if side.as_deref().is_some_and(|s| s != "me" && s != "opp") {
        return Err(bad("side is me or opp"));
    }
    let cards: Option<HashSet<String>> = match p.cards.filter(|c| !c.trim().is_empty()) {
        Some(c) => Some(serde_json::from_str::<Vec<String>>(&c).map_err(|e| bad(format!("cards: {e}")))?.into_iter().collect()).filter(|c: &HashSet<String>| !c.is_empty()),
        None => None,
    };
    let r = tokio::task::spawn_blocking(move || -> Result<CardCounts, String> {
        use diesel::prelude::*;
        let conn = &mut reader(ds, &st.data, &names)?;
        let sel: SelCount = diesel::sql_query(format!("SELECT COUNT(*) AS games, COALESCE(SUM(my_hand_known), 0) AS hand FROM mtgo_game_view {wher}")).get_result(conn).map_err(|e| format!("query failed: {e}"))?;
        let hand_verbs = VERBS.iter().filter(|(_, _, n)| *n == Some("hand")).map(|(v, _, _)| sq(v)).collect::<Vec<_>>().join(", ");
        let event_verbs: Vec<&String> = verbs.iter().filter(|v| v.as_str() != "deck").collect();
        let mut parts = vec![];
        if verbs.is_empty() || !event_verbs.is_empty() {
            let mut conds = vec![format!("(c.verb NOT IN ({hand_verbs}) OR s.my_hand_known = 1)"), "(c.verb <> 'pile' OR s.my_pile_known = 1)".to_string()];
            if !event_verbs.is_empty() {
                conds.push(format!("c.verb IN ({})", event_verbs.iter().map(|v| sq(v)).collect::<Vec<_>>().join(", ")));
            }
            if let Some(sd) = &side {
                conds.push(format!("c.side = {}", sq(sd)));
            }
            parts.push(format!("SELECT c.game_id, c.side, c.verb, c.card, COUNT(*) AS n FROM mtgo_card_events c JOIN sel s ON s.game_id = c.game_id WHERE {} GROUP BY c.game_id, c.side, c.verb, c.card", conds.join(" AND ")));
        }
        if (verbs.is_empty() || verbs.iter().any(|v| v == "deck")) && side.as_deref() != Some("opp") {
            parts.push(format!("SELECT game_id, 'me' AS side, 'deck' AS verb, card, n FROM ({})", deck_sql("sel")));
        }
        let raw: Vec<CardCountRow> = diesel::sql_query(format!(
            "WITH sel AS (SELECT game_id, game_list_id, my_hand_known, my_pile_known FROM mtgo_game_view {wher}) {}",
            parts.join(" UNION ALL ")
        ))
        .load(conn)
        .map_err(|e| format!("query failed: {e}"))?;
        let keep = |r: &&CardCountRow| cards.as_ref().map_or(true, |c| c.contains(&r.card));
        let mut rows: Vec<CardCount>;
        let totals: Vec<CardCount>;
        if p.by_game {
            rows = raw.iter().filter(keep).map(|r| CardCount { game_id: Some(r.game_id), side: r.side.clone(), verb: r.verb.clone(), card: Some(r.card.clone()), n: r.n, games: None }).collect();
            rows.sort_by(|a, b| b.game_id.cmp(&a.game_id).then(a.side.cmp(&b.side)).then(a.verb.cmp(&b.verb)).then(b.n.cmp(&a.n)).then(a.card.cmp(&b.card)));
            let mut t: BTreeMap<(std::cmp::Reverse<i32>, &str, &str), i64> = BTreeMap::new();
            for r in &raw {
                *t.entry((std::cmp::Reverse(r.game_id), &r.side, &r.verb)).or_default() += r.n;
            }
            totals = t.into_iter().map(|((g, sd, v), n)| CardCount { game_id: Some(g.0), side: sd.into(), verb: v.into(), card: None, n, games: None }).collect();
        } else {
            let mut a: BTreeMap<(&str, &str, &str), (i64, i64)> = BTreeMap::new();
            for r in raw.iter().filter(keep) {
                let e = a.entry((&r.side, &r.verb, &r.card)).or_default();
                e.0 += r.n;
                e.1 += 1;
            }
            rows = a.into_iter().map(|((sd, v, c), (n, g))| CardCount { game_id: None, side: sd.into(), verb: v.into(), card: Some(c.into()), n, games: Some(g) }).collect();
            rows.sort_by(|a, b| a.side.cmp(&b.side).then(a.verb.cmp(&b.verb)).then(b.n.cmp(&a.n)).then(a.card.cmp(&b.card)));
            let mut t: BTreeMap<(&str, &str), (i64, HashSet<i32>)> = BTreeMap::new();
            for r in &raw {
                let e = t.entry((&r.side, &r.verb)).or_default();
                e.0 += r.n;
                e.1.insert(r.game_id);
            }
            totals = t.into_iter().map(|((sd, v), (n, g))| CardCount { game_id: None, side: sd.into(), verb: v.into(), card: None, n, games: Some(g.len() as i64) }).collect();
        }
        let truncated = rows.len() > p.limit;
        rows.truncate(p.limit);
        Ok(CardCounts { games: sel.games, games_hand_known: sel.hand, rows, totals, truncated })
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(bad)?;
    Ok(Json(r))
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
            let date_of: HashMap<i32, &str> = truth.iter().map(|t| (t.match_id, t.date.as_str())).collect();
            for (id, gs) in games.iter_mut() {
                r::cap_revealed(gs, refs.at(date_of.get(id).copied().unwrap_or("")));
            }
            if pool_run {
                games = r::pool_runs(&truth.iter().map(|t| (t.match_id, t.date.clone())).collect::<Vec<_>>(), &games, &refs).into_iter().map(|(k, (g, _))| (k, g)).collect();
            }
            if side == "me" {
                // list-level check: with the player's lists as hypotheses, does a match linked by the session log land on its own list?
                let lists = r::player_lists(conn, &refs.fetches, &player).unwrap();
                let with_lists = refs.with_lists_all(&lists);
                #[derive(diesel::QueryableByName)]
                struct Linked { #[diesel(sql_type = diesel::sql_types::Integer)] match_id: i32, #[diesel(sql_type = diesel::sql_types::Integer)] list_id: i32, #[diesel(sql_type = diesel::sql_types::Text)] date: String }
                let linked: Vec<Linked> = diesel::sql_query("SELECT id AS match_id, my_list_id AS list_id, COALESCE(substr(start_ts,1,10), '') AS date FROM mtgo_matches WHERE my_list_id IS NOT NULL AND replay = 0").load(conn).unwrap();
                let (mut lr, mut lw, mut ln) = (0, 0, 0);
                let mut misses: Vec<String> = vec![];
                let by_id: HashMap<i32, &r::RefList> = lists.iter().map(|l| (-l.list.id, &l.list)).collect();
                for l in &linked {
                    let Some(gs) = games.get(&l.match_id) else { continue };
                    match r::infer_list(gs, &l.date, &with_lists[refs.snapshot_index(&l.date).unwrap_or(0)]) {
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
                let inf = r::infer(gs, refs.at(&t.date));
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
        Cmd::Relabel { player } => {
            use mtgctl::mtgolog::reference as r;
            let shared = &mut shared_db(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let refs = r::load_references(shared, &timeline_eras()).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let conn = &mut pipeline::connect(&cli.data).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            mtgctl::mtgolog::db::ensure_schema(conn).and_then(|_| stats::ensure_view(conn)).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let markers = mtgctl::mtgolog::legality::markers(&r::load_legacy_illegal(shared).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) }), &mtgctl::mtgolog::eras::load_timeline(Path::new("definitions/mtgo/legacy_timeline.toml")).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) }));
            let lg = mtgctl::mtgolog::legality::stamp(conn, &player, &markers).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let assigned = r::assign_player(conn, &refs, &player).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            let md = r::label_matches(conn, &refs, &player).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            println!("{player}: format: {} matches named by the session log, {} not Legacy; {} of {} lists named by {} reference decklists; match decks: mine from list {}, from cards {} ({} placed on a registered list); opponents named {}, archetype only {}", lg.session, lg.not_legacy, assigned.iter().filter(|a| a.deck.is_some()).count(), assigned.len(), refs.refs.len(), md.me_list, md.me_cards, md.me_list_inferred, md.opp_named, md.opp_archetype_only);
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
        Cmd::Eras { repo, all } => {
            if let Err(e) = eras_report(&cli.data, &Reference::locate(&repo, &cli.data).timeline, all) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Cmd::LdcSync { repo, all } => {
            let timeline = Reference::locate(&repo, &cli.data).timeline;
            let run = || -> Result<(), String> {
                println!("{} sheets downloaded", ldc::fetch(&cli.data, all)?);
                // a source that is down today is tried again tomorrow; the rebuild goes ahead
                match ldc::fetch_lists(&cli.data) {
                    Ok((pages, lists)) => println!("{pages} event pages and {lists} lists downloaded"),
                    Err(e) => eprintln!("ldc: {e}"),
                }
                let eras = mtgctl::mtgolog::eras::eras_from_file(&timeline)?;
                let refs = mtgctl::mtgolog::reference::load_references(&mut shared_db(&cli.data)?, &eras).map_err(|e| e.to_string())?;
                let r = ldc::import(&cli.data, &timeline, &refs)?;
                println!("ldc.db: {} events, {} matches (one per side), {} .. {}; {} published lists, {} placed on a reference deck", r.events, r.matches, r.first, r.last, r.lists, r.lists_named);
                for s in &r.skipped {
                    println!("  skipped {s}");
                }
                Ok(())
            };
            if let Err(e) = run() {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Cmd::Accounts | Cmd::Bind { .. } | Cmd::Unbind { .. } => {
            let r = auth::ensure(&cli.data).and_then(|_| match &cli.command {
                Cmd::Bind { account, name } => auth::cli_bind(&cli.data, account, name),
                Cmd::Unbind { name } => auth::cli_unbind(&cli.data, name),
                _ => auth::cli_accounts(&cli.data),
            });
            if let Err(e) = r {
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
        Cmd::Serve { listen, web, repo, auth } => {
            let auth = auth::Config::load(&auth).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1) });
            std::fs::create_dir_all(cli.data.join("players")).expect("data dir");
            auth::ensure(&cli.data).expect("accounts database");
            // once here, so that requests only read
            let conn = &mut pipeline::connect(&cli.data).expect("database");
            mtgctl::mtgolog::db::ensure_schema(conn).expect("schema");
            stats::ensure_view(conn).expect("view");
            for ds in dataset::ALL {
                ds.open(&cli.data).expect("dataset tables and view");
            }
            let reference = Reference::locate(&repo, &cli.data);
            // a deploy that changed the timeline re-places the matches from the first changed era on
            match mtgctl::mtgolog::eras::stamp(conn, &reference.timeline, false) {
                Ok(s) => eprintln!("eras: {} matches placed in {} eras{}", s.placed, s.eras, s.from.map(|d| format!(", the timeline changed from {d} on")).unwrap_or_default()),
                Err(e) => eprintln!("eras: {e}"),
            }
            let state = Arc::new(AppState { data: cli.data.clone(), reference, sync_lock: Mutex::new(()), auth });
            let app = Router::new()
                .route("/auth/login", get(auth::login))
                .route("/auth/callback", get(auth::callback))
                .route("/auth/logout", post(auth::logout))
                .route("/api/account", get(auth::account_api))
                .route("/api/keys", post(auth::key_new))
                .route("/api/keys/revoke", post(auth::key_revoke))
                .route("/api/teams/{action}", post(auth::team_action))
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
                .route("/api/replay", get(replay_api))
                .route("/api/columns", get(columns))
                .route("/api/values", get(values))
                .route("/api/schema", get(schema))
                .route("/api/cards-seen", get(cards_seen))
                .route("/api/card-counts", get(card_counts))
                .route("/api/list/experimental", post(list_experimental))
                .route("/api/mark", post(mark_api))
                .route("/api/decks", get(decks_api))
                .route("/api/lists", get(lists))
                .route("/api/list", get(list_detail))
                .route("/mcp", post(mcp::mcp).get(mcp::mcp_get))
                .fallback_service(ServeDir::new(&web))
                .layer(axum::middleware::from_fn(plain_text_scripts))
                .layer(axum::middleware::from_fn(no_sql_params))
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
