//! Logins and uploader keys. A Discord login (OAuth2 authorization-code flow, scope `identify`)
//! makes an account and a session cookie; a key made on the upload page stands in for the cookie
//! in the uploaders. `<data>/accounts.db` holds the accounts, the MTGO names bound to them,
//! sessions and keys; a session or key is stored only as its SHA-256. The client secret comes from
//! the `--auth` file on the server, never from git.

use super::*;
use axum::body::Body;
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Nullable, Text};
use rand::RngCore;

/// The `--auth` file; see deploy/auth.toml.example.
#[derive(Deserialize)]
pub struct Config {
    client_id: String,
    client_secret: String,
    /// a redirect registered on the Discord application, ending in `auth/callback`
    redirect_uri: String,
    /// MTGO names that may upload without a login or key (uploaders installed before logins)
    #[serde(default)]
    keyless: Vec<String>,
    /// Discord's API and authorization page; a test points them at a stand-in
    #[serde(default = "discord_api")]
    api: String,
    #[serde(default = "discord_authorize")]
    authorize: String,
    /// scheme and host of `redirect_uri`: the only Origin a cookie-authenticated write may carry
    #[serde(skip)]
    origin: String,
    /// the path `redirect_uri` lives under, ending in `/`: the cookies' path
    #[serde(skip)]
    prefix: String,
}

fn discord_api() -> String {
    "https://discord.com/api/v10".into()
}
fn discord_authorize() -> String {
    "https://discord.com/oauth2/authorize".into()
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e} (see deploy/auth.toml.example)", path.display()))?;
        let mut c: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let bad = || format!("{}: redirect_uri must look like https://host/path/auth/callback", path.display());
        let (scheme, rest) = c.redirect_uri.split_once("://").ok_or_else(bad)?;
        let slash = rest.find('/').ok_or_else(bad)?;
        c.origin = format!("{scheme}://{}", &rest[..slash]);
        c.prefix = rest[slash..].strip_suffix("auth/callback").ok_or_else(bad)?.to_string();
        Ok(c)
    }
    fn secure(&self) -> &'static str {
        if self.redirect_uri.starts_with("https://") {
            "; Secure"
        } else {
            ""
        }
    }
    fn keyless(&self, name: &str) -> bool {
        self.keyless.iter().any(|k| k.eq_ignore_ascii_case(name))
    }
}

const DDL: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS accounts (
        id INTEGER PRIMARY KEY,
        discord_id TEXT NOT NULL UNIQUE,
        name TEXT NOT NULL,              -- the Discord display name at the last login
        created TEXT NOT NULL,
        last_login TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS account_names (
        name TEXT PRIMARY KEY COLLATE NOCASE,   -- an MTGO username: its uploads belong to the account
        account_id INTEGER NOT NULL REFERENCES accounts(id),
        how TEXT NOT NULL,               -- upload (claimed by its first upload) | admin
        at TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS sessions (
        token_hash TEXT PRIMARY KEY,
        account_id INTEGER NOT NULL REFERENCES accounts(id),
        expires TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS upload_keys (
        id INTEGER PRIMARY KEY,
        token_hash TEXT NOT NULL UNIQUE,
        account_id INTEGER NOT NULL REFERENCES accounts(id),
        created TEXT NOT NULL,
        last_used TEXT,
        revoked TEXT
    )",
];

const SESSION_COOKIE: &str = "mtgctl_session";
const LOGIN_COOKIE: &str = "mtgctl_login";
const SESSION_DAYS: i64 = 90;

pub fn ensure(data: &Path) -> Result<(), String> {
    let conn = &mut conn(data)?;
    for d in DDL {
        diesel::sql_query(*d).execute(conn).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn conn(data: &Path) -> Result<SqliteConnection, String> {
    mtgctl::db::establish_connection_at(&data.join("accounts.db"))
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 32 random bytes from the OS as hex, after `prefix`.
fn token(prefix: &str) -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("{prefix}{}", hex(&b))
}

fn hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

#[derive(diesel::QueryableByName)]
struct Id {
    #[diesel(sql_type = BigInt)]
    id: i64,
}

#[derive(diesel::QueryableByName)]
struct NameRow {
    #[diesel(sql_type = Text)]
    name: String,
}

#[derive(Clone, Serialize)]
pub struct Account {
    pub id: i64,
    /// the Discord display name
    pub name: String,
    /// the MTGO names bound to the account
    pub names: Vec<String>,
}

fn account(conn: &mut SqliteConnection, id: i64) -> Result<Option<Account>, String> {
    let name: Option<NameRow> = diesel::sql_query(format!("SELECT name FROM accounts WHERE id = {id}")).get_result(conn).optional().map_err(|e| e.to_string())?;
    let Some(name) = name else { return Ok(None) };
    let names: Vec<NameRow> = diesel::sql_query(format!("SELECT name FROM account_names WHERE account_id = {id} ORDER BY name")).load(conn).map_err(|e| e.to_string())?;
    Ok(Some(Account { id, name: name.name, names: names.into_iter().map(|n| n.name).collect() }))
}

fn owner(conn: &mut SqliteConnection, name: &str) -> Result<Option<i64>, String> {
    let r: Option<Id> = diesel::sql_query(format!("SELECT account_id AS id FROM account_names WHERE name = {}", sq(name))).get_result(conn).optional().map_err(|e| e.to_string())?;
    Ok(r.map(|r| r.id))
}

/// Who a request comes from: the account of its `Authorization: Bearer` key or of its session
/// cookie, or nobody.
pub struct Viewer {
    pub account: Option<Account>,
    /// the account came from a session cookie, not a key
    pub session: bool,
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).filter_map(|kv| kv.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
}

fn is_token(t: &str, prefix: &str) -> bool {
    t.strip_prefix(prefix).is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `write`: the handler changes something. A cookie-authenticated write must carry the site's own
/// Origin, so a page elsewhere cannot make one with the visitor's cookie.
pub async fn viewer(st: &Shared, headers: &HeaderMap, write: bool) -> Result<Viewer, ApiError> {
    let key = match headers.get(axum::http::header::AUTHORIZATION).map(|v| v.to_str().unwrap_or("")) {
        None => None,
        Some(v) => Some(v.strip_prefix("Bearer ").filter(|k| is_token(k, "mk_")).ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "malformed uploader key".into()))?.to_string()),
    };
    let session = cookie(headers, SESSION_COOKIE).filter(|s| is_token(s, ""));
    if key.is_none() && session.is_none() {
        return Ok(Viewer { account: None, session: false });
    }
    if key.is_none() && write && headers.get(axum::http::header::ORIGIN).and_then(|o| o.to_str().ok()) != Some(st.auth.origin.as_str()) {
        return Err(ApiError(StatusCode::FORBIDDEN, "a change must come from this site's own pages".into()));
    }
    let data = st.data.clone();
    tokio::task::spawn_blocking(move || -> Result<Viewer, ApiError> {
        let conn = &mut conn(&data).map_err(internal)?;
        if let Some(k) = key {
            let r: Option<Id> = diesel::sql_query(format!("SELECT account_id AS id FROM upload_keys WHERE token_hash = '{}' AND revoked IS NULL", hash(&k))).get_result(conn).optional().map_err(|e| internal(e.to_string()))?;
            let id = r.ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "unknown or revoked uploader key: make a new install line on the upload page".into()))?.id;
            diesel::sql_query(format!("UPDATE upload_keys SET last_used = '{}' WHERE token_hash = '{}'", now(), hash(&k))).execute(conn).map_err(|e| internal(e.to_string()))?;
            return Ok(Viewer { account: account(conn, id).map_err(internal)?, session: false });
        }
        let s = session.unwrap();
        let r: Option<Id> = diesel::sql_query(format!("SELECT account_id AS id FROM sessions WHERE token_hash = '{}' AND expires > '{}'", hash(&s), now())).get_result(conn).optional().map_err(|e| internal(e.to_string()))?;
        Ok(match r {
            Some(r) => Viewer { account: account(conn, r.id).map_err(internal)?, session: true },
            None => Viewer { account: None, session: false },
        })
    })
    .await
    .map_err(|e| internal(e.to_string()))?
}

/// How a request may use an MTGO name's uploads (store files, read what is stored, sync).
#[derive(PartialEq, Debug)]
pub enum Access {
    /// the name is bound to the viewer's account
    Owner,
    /// the name is in the config's `keyless` list
    Keyless,
    /// the name is bound to nobody and has no uploads: an upload whose match logs name it binds
    /// it to the viewer's account (`claim`)
    Claim(i64),
}

/// The MTGO name a request is about (`X-Player`) and how the viewer may use it. A name with
/// uploads and no account is bound only by the admin (`bind`): a forged log could otherwise hand
/// over its history, hands included.
pub async fn player_access(st: &Shared, headers: &HeaderMap, q: &HashMap<String, String>, write: bool) -> Result<(String, Access), ApiError> {
    let name = auth(st, headers, q)?;
    let v = viewer(st, headers, write).await?;
    let (st2, n) = (st.clone(), name.clone());
    let owner = tokio::task::spawn_blocking(move || owner(&mut conn(&st2.data)?, &n)).await.map_err(|e| internal(e.to_string()))?.map_err(internal)?;
    let me = v.account.as_ref().map(|a| a.id);
    let access = match (owner, me) {
        (Some(o), Some(m)) if o == m => Access::Owner,
        _ if st.auth.keyless(&name) => Access::Keyless,
        (Some(_), _) => return Err(ApiError(StatusCode::FORBIDDEN, format!("{name} belongs to another account"))),
        (None, None) => return Err(ApiError(StatusCode::UNAUTHORIZED, format!("log in on the upload page (or install the uploader with your own install line) to upload as {name}"))),
        (None, Some(m)) if stored_files(&player_dir(st, &name)).is_empty() => Access::Claim(m),
        (None, Some(_)) => return Err(ApiError(StatusCode::FORBIDDEN, format!("{name} has uploads from before logins: ask the admin to link it to your account"))),
    };
    Ok((name, access))
}

/// Binds `name` to `account` when the game logs of the upload name it as the one player in all of
/// them (at least two must parse: in a single log both players are).
pub fn claim(data: &Path, account: i64, name: &str, logs: &[&[u8]]) -> Result<(), String> {
    let mut common: Option<HashSet<String>> = None;
    let mut parsed = 0;
    for b in logs {
        let Ok(raw) = mtgctl::mtgolog::format::parse_bytes(b) else { continue };
        parsed += 1;
        let players: HashSet<String> = mtgctl::mtgolog::model::build_match(&raw).players.into_iter().collect();
        common = Some(match common {
            None => players,
            Some(c) => c.intersection(&players).cloned().collect(),
        });
    }
    let common = common.unwrap_or_default();
    if parsed < 2 || common.len() != 1 {
        return Err(format!("the first upload as {name} must hold at least two of your match logs, so the server can tell you from your opponents"));
    }
    if !common.iter().any(|c| c.eq_ignore_ascii_case(name)) {
        return Err(format!("these match logs belong to {}, not {name}", common.into_iter().next().unwrap()));
    }
    let conn = &mut conn(data)?;
    diesel::sql_query(format!("INSERT INTO account_names(name, account_id, how, at) VALUES ({}, {account}, 'upload', '{}')", sq(name), now())).execute(conn).map_err(|_| format!("{name} was just linked to another account"))?;
    Ok(())
}

/// The account to show on the pages, and its uploader keys when the request has a session.
pub async fn account_api(State(st): State<Shared>, headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let v = viewer(&st, &headers, false).await?;
    let Some(a) = v.account else { return Ok(Json(serde_json::json!({ "account": null }))) };
    let keys = if v.session { list_keys(&st, a.id).await? } else { vec![] };
    Ok(Json(serde_json::json!({ "account": a, "keys": keys })))
}

#[derive(diesel::QueryableByName, Serialize)]
pub struct KeyRow {
    #[diesel(sql_type = BigInt)]
    id: i64,
    #[diesel(sql_type = Text)]
    created: String,
    #[diesel(sql_type = Nullable<Text>)]
    last_used: Option<String>,
}

async fn list_keys(st: &Shared, account: i64) -> Result<Vec<KeyRow>, ApiError> {
    let data = st.data.clone();
    tokio::task::spawn_blocking(move || diesel::sql_query(format!("SELECT id, created, last_used FROM upload_keys WHERE account_id = {account} AND revoked IS NULL ORDER BY id")).load(&mut conn(&data)?).map_err(|e| e.to_string()))
        .await
        .map_err(|e| internal(e.to_string()))?
        .map_err(internal)
}

/// Keys are made and revoked only with a session: a key cannot make more keys.
async fn session_account(st: &Shared, headers: &HeaderMap) -> Result<Account, ApiError> {
    let v = viewer(st, headers, true).await?;
    match v.account {
        Some(a) if v.session => Ok(a),
        _ => Err(ApiError(StatusCode::UNAUTHORIZED, "log in first".into())),
    }
}

/// A new uploader key, shown this once.
pub async fn key_new(State(st): State<Shared>, headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let a = session_account(&st, &headers).await?;
    let key = token("mk_");
    let (data, h) = (st.data.clone(), hash(&key));
    let id = tokio::task::spawn_blocking(move || -> Result<i64, String> {
        let conn = &mut conn(&data)?;
        diesel::sql_query(format!("INSERT INTO upload_keys(token_hash, account_id, created) VALUES ('{h}', {}, '{}')", a.id, now())).execute(conn).map_err(|e| e.to_string())?;
        Ok(diesel::sql_query("SELECT last_insert_rowid() AS id").get_result::<Id>(conn).map_err(|e| e.to_string())?.id)
    })
    .await
    .map_err(|e| internal(e.to_string()))?
    .map_err(internal)?;
    Ok(Json(serde_json::json!({ "id": id, "key": key })))
}

#[derive(Deserialize)]
pub struct RevokeReq {
    id: i64,
}

pub async fn key_revoke(State(st): State<Shared>, headers: HeaderMap, Json(req): Json<RevokeReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let a = session_account(&st, &headers).await?;
    let data = st.data.clone();
    let n = tokio::task::spawn_blocking(move || diesel::sql_query(format!("UPDATE upload_keys SET revoked = '{}' WHERE id = {} AND account_id = {} AND revoked IS NULL", now(), req.id, a.id)).execute(&mut conn(&data)?).map_err(|e| e.to_string()))
        .await
        .map_err(|e| internal(e.to_string()))?
        .map_err(internal)?;
    if n == 0 {
        return Err(bad(format!("no key {}", req.id)));
    }
    Ok(Json(serde_json::json!({ "revoked": req.id })))
}

fn pct(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A page name the login returns to: `[a-z]+.html`, nothing that could leave the site.
fn valid_next(n: &str) -> bool {
    n.strip_suffix(".html").is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()))
}

fn redirect(to: &str, cookies: &[String]) -> Response {
    let mut r = Response::builder().status(StatusCode::FOUND).header(LOCATION, to);
    for c in cookies {
        r = r.header(SET_COOKIE, c);
    }
    r.body(Body::empty()).unwrap()
}

/// `auth/login?next=page.html`: off to Discord, with a one-time state in a short cookie.
pub async fn login(State(st): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let c = &st.auth;
    let next = q.get("next").filter(|n| valid_next(n)).map_or("index.html", String::as_str);
    let state = token("");
    let url = format!("{}?client_id={}&response_type=code&redirect_uri={}&scope=identify&state={state}", c.authorize, pct(&c.client_id), pct(&c.redirect_uri));
    redirect(&url, &[format!("{LOGIN_COOKIE}={state}.{next}; Path={}auth/; Max-Age=600; HttpOnly; SameSite=Lax{}", c.prefix, c.secure())])
}

/// `auth/callback?code&state` from Discord: the account (made on its first login) gets a session.
pub async fn callback(State(st): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let c = &st.auth;
    let clear_login = format!("{LOGIN_COOKIE}=; Path={}auth/; Max-Age=0; HttpOnly; SameSite=Lax{}", c.prefix, c.secure());
    match finish_login(&st, &headers, &q).await {
        Ok((session, next)) => redirect(&format!("{}{next}", c.prefix), &[format!("{SESSION_COOKIE}={session}; Path={}; Max-Age={}; HttpOnly; SameSite=Lax{}", c.prefix, SESSION_DAYS * 86400, c.secure()), clear_login]),
        Err(msg) => {
            eprintln!("login: {msg}");
            let page = format!("<!doctype html><meta charset=\"utf-8\"><title>Login failed · mtgctl</title><link rel=\"stylesheet\" href=\"{p}style.css\"><h1>Login failed</h1><p>{}</p><p><a href=\"{p}collect.html\">back to the upload page</a></p>", html_escape(&msg), p = c.prefix);
            Response::builder().status(StatusCode::BAD_REQUEST).header(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8").header(SET_COOKIE, clear_login).body(Body::from(page)).unwrap()
        }
    }
}

async fn finish_login(st: &Shared, headers: &HeaderMap, q: &HashMap<String, String>) -> Result<(String, String), String> {
    if let Some(e) = q.get("error") {
        return Err(format!("Discord answered: {}", q.get("error_description").unwrap_or(e)));
    }
    let (state, next) = cookie(headers, LOGIN_COOKIE).and_then(|v| v.split_once('.').map(|(s, n)| (s.to_string(), n.to_string()))).ok_or("the login took too long or cookies are blocked; try again")?;
    if q.get("state") != Some(&state) || !valid_next(&next) {
        return Err("this login was started elsewhere; try again".into());
    }
    let code = q.get("code").ok_or("Discord sent no code")?.clone();
    let st = st.clone();
    tokio::task::spawn_blocking(move || -> Result<(String, String), String> {
        let c = &st.auth;
        let form = format!("grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&client_secret={}", pct(&code), pct(&c.redirect_uri), pct(&c.client_id), pct(&c.client_secret));
        let tok = curl_json(&["-X", "POST", "-H", "Content-Type: application/x-www-form-urlencoded", "--data-binary", "@-", &format!("{}/oauth2/token", c.api)], &form)?;
        let access = tok["access_token"].as_str().ok_or("Discord sent no access token")?;
        let user = curl_json(&["-H", "@-", &format!("{}/users/@me", c.api)], &format!("Authorization: Bearer {access}\n"))?;
        let id = user["id"].as_str().filter(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit())).ok_or("Discord sent no user id")?;
        let name = user["global_name"].as_str().or(user["username"].as_str()).unwrap_or(id);
        let conn = &mut conn(&st.data)?;
        let t = now();
        diesel::sql_query(format!("INSERT INTO accounts(discord_id, name, created, last_login) VALUES ('{id}', {}, '{t}', '{t}') ON CONFLICT(discord_id) DO UPDATE SET name = excluded.name, last_login = excluded.last_login", sq(name))).execute(conn).map_err(|e| e.to_string())?;
        let account = diesel::sql_query(format!("SELECT id FROM accounts WHERE discord_id = '{id}'")).get_result::<Id>(conn).map_err(|e| e.to_string())?.id;
        let session = token("");
        let expires = (chrono::Utc::now() + chrono::Duration::days(SESSION_DAYS)).format("%Y-%m-%d %H:%M:%S");
        diesel::sql_query(format!("DELETE FROM sessions WHERE expires <= '{t}'")).execute(conn).map_err(|e| e.to_string())?;
        diesel::sql_query(format!("INSERT INTO sessions(token_hash, account_id, expires) VALUES ('{}', {account}, '{expires}')", hash(&session))).execute(conn).map_err(|e| e.to_string())?;
        eprintln!("login: account {account} ({name}, discord {id})");
        Ok((session, next))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `curl` reading `stdin` (the request body, or the headers for `-H @-`), so neither the secret nor
/// a token is on a command line; the JSON of a 200 reply.
fn curl_json(args: &[&str], stdin: &str) -> Result<serde_json::Value, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", "20", "-A", "DiscordBot (https://bur.io/mtg/stats, 1)", "-w", "\n%{http_code}"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).map_err(|e| format!("curl: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, ""));
    if code != "200" {
        return Err(format!("Discord answered {code}: {}", body.chars().take(300).collect::<String>()));
    }
    serde_json::from_str(body).map_err(|e| format!("Discord: {e}"))
}

pub async fn logout(State(st): State<Shared>, headers: HeaderMap) -> Result<Response, ApiError> {
    if let Some(s) = cookie(&headers, SESSION_COOKIE).filter(|s| is_token(s, "")) {
        viewer(&st, &headers, true).await?;
        let data = st.data.clone();
        tokio::task::spawn_blocking(move || diesel::sql_query(format!("DELETE FROM sessions WHERE token_hash = '{}'", hash(&s))).execute(&mut conn(&data)?).map_err(|e| e.to_string()))
            .await
            .map_err(|e| internal(e.to_string()))?
            .map_err(internal)?;
    }
    let c = &st.auth;
    Ok(Response::builder().status(StatusCode::NO_CONTENT).header(SET_COOKIE, format!("{SESSION_COOKIE}=; Path={}; Max-Age=0; HttpOnly; SameSite=Lax{}", c.prefix, c.secure())).body(Body::empty()).unwrap())
}

/// `accounts`: every account, its MTGO names and live keys.
pub fn cli_accounts(data: &Path) -> Result<(), String> {
    #[derive(diesel::QueryableByName)]
    struct R {
        #[diesel(sql_type = BigInt)]
        id: i64,
        #[diesel(sql_type = Text)]
        discord_id: String,
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = Text)]
        last_login: String,
        #[diesel(sql_type = Nullable<Text>)]
        names: Option<String>,
        #[diesel(sql_type = BigInt)]
        keys: i64,
    }
    let rows: Vec<R> = diesel::sql_query(
        "SELECT a.id, a.discord_id, a.name, a.last_login,
                (SELECT GROUP_CONCAT(n.name || ' (' || n.how || ')', ', ') FROM account_names n WHERE n.account_id = a.id) AS names,
                (SELECT COUNT(*) FROM upload_keys k WHERE k.account_id = a.id AND k.revoked IS NULL) AS keys
         FROM accounts a ORDER BY a.id",
    )
    .load(&mut conn(data)?)
    .map_err(|e| e.to_string())?;
    println!("{:>3}  {:<20} {:<24} {:<19}  {:>4}  MTGO names", "id", "discord id", "name", "last login", "keys");
    for r in rows {
        println!("{:>3}  {:<20} {:<24} {:<19}  {:>4}  {}", r.id, r.discord_id, r.name, r.last_login, r.keys, r.names.unwrap_or_default());
    }
    Ok(())
}

/// `bind <account> <name>`: the account by id, Discord id or Discord name; replaces any binding.
pub fn cli_bind(data: &Path, who: &str, name: &str) -> Result<(), String> {
    if !valid_player(name) {
        return Err(format!("{name:?} is not an MTGO username"));
    }
    let conn = &mut conn(data)?;
    let ids: Vec<Id> = diesel::sql_query(format!("SELECT id FROM accounts WHERE CAST(id AS TEXT) = {w} OR discord_id = {w} OR name = {w} COLLATE NOCASE", w = sq(who))).load(conn).map_err(|e| e.to_string())?;
    let [Id { id }] = ids[..] else { return Err(format!("{} accounts match {who:?}; see `accounts`", ids.len())) };
    let before = owner(conn, name)?;
    diesel::sql_query(format!("INSERT OR REPLACE INTO account_names(name, account_id, how, at) VALUES ({}, {id}, 'admin', '{}')", sq(name), now())).execute(conn).map_err(|e| e.to_string())?;
    let uploads = stored_files(&player_dir_at(data, name)).len();
    println!("{name} → account {id}{}; {uploads} uploaded files under that name", before.filter(|b| *b != id).map(|b| format!(" (was account {b})")).unwrap_or_default());
    Ok(())
}

pub fn cli_unbind(data: &Path, name: &str) -> Result<(), String> {
    let n = diesel::sql_query(format!("DELETE FROM account_names WHERE name = {}", sq(name))).execute(&mut conn(data)?).map_err(|e| e.to_string())?;
    println!("{name}: {}", if n > 0 { "no longer linked to an account" } else { "was not linked" });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_derives_origin_and_prefix() {
        let dir = std::env::temp_dir().join(format!("mtgctl-auth-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("auth.toml");
        std::fs::write(&p, "client_id = \"1\"\nclient_secret = \"s\"\nredirect_uri = \"https://bur.io/mtg/stats/auth/callback\"\nkeyless = [\"boryas\"]\n").unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!((c.origin.as_str(), c.prefix.as_str(), c.secure()), ("https://bur.io", "/mtg/stats/", "; Secure"));
        assert!(c.keyless("Boryas") && !c.keyless("Jaytron"));
        std::fs::write(&p, "client_id = \"1\"\nclient_secret = \"s\"\nredirect_uri = \"https://bur.io/mtg/stats/callback\"\n").unwrap();
        assert!(Config::load(&p).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn next_pages_stay_on_the_site() {
        assert!(valid_next("index.html") && valid_next("collect.html"));
        for n in ["", ".html", "https://evil.example/x.html", "//evil.example", "../x.html", "Index.html", "index.htm"] {
            assert!(!valid_next(n), "{n}");
        }
    }

    #[test]
    fn tokens() {
        let k = token("mk_");
        assert!(is_token(&k, "mk_") && !is_token(&k, "") && is_token(&token(""), ""));
        assert_ne!(token(""), token(""));
        assert_eq!(pct("a b/é"), "a%20b%2F%C3%A9");
    }
}
