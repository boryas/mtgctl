//! Hand marks: the one input not derived from the logs. A match mark names its opponent deck,
//! my deck or format, adds a note, or excludes the match from the stats; a list mark is the
//! experimental tag. Every change is appended to `<data>/marks.jsonl` and applied to the pooled
//! database, and `apply` replays the file, which is why a rebuild keeps them. Nothing checks who
//! marks what (user, 2026-11-13: the players know each other; identity comes later).

use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MATCH_FIELDS: &[&str] = &["opp_deck", "my_deck", "format", "note", "exclude"];
pub const LIST_FIELDS: &[&str] = &["experimental"];

pub const DDL: &str = "CREATE TABLE IF NOT EXISTS match_marks (
    me TEXT NOT NULL,
    guid TEXT NOT NULL,              -- mtgo_matches.guid, the identity a re-import keeps
    field TEXT NOT NULL,             -- see MATCH_FIELDS
    value TEXT NOT NULL,
    extra TEXT,                      -- a deck mark's archetype
    by TEXT NOT NULL,
    at TEXT NOT NULL,
    PRIMARY KEY (me, guid, field)
)";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Mark {
    pub at: String,
    pub by: String,
    /// the match's or list's player
    pub me: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub field: String,
    /// None clears the field
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
}

pub fn path(data: &Path) -> PathBuf {
    data.join("marks.jsonl")
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Append the mark to the file, then apply it.
pub fn record(conn: &mut SqliteConnection, data: &Path, mark: &Mark) -> Result<(), String> {
    let valid = match (&mark.guid, &mark.hash) {
        (Some(_), None) => MATCH_FIELDS.contains(&mark.field.as_str()),
        (None, Some(_)) => LIST_FIELDS.contains(&mark.field.as_str()),
        _ => false,
    };
    if !valid {
        return Err(format!("not a mark: {} on {}", mark.field, if mark.guid.is_some() { "a match" } else { "a list" }));
    }
    let mut line = serde_json::to_string(mark).map_err(|e| e.to_string())?;
    line.push('\n');
    use std::io::Write;
    let p = path(data);
    std::fs::OpenOptions::new().create(true).append(true).open(&p).and_then(|mut f| f.write_all(line.as_bytes())).map_err(|e| format!("{}: {e}", p.display()))?;
    apply_one(conn, mark).map_err(|e| e.to_string())
}

fn apply_one(conn: &mut SqliteConnection, m: &Mark) -> QueryResult<()> {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let o = |s: &Option<String>| s.as_deref().map(q).unwrap_or_else(|| "NULL".into());
    if let Some(guid) = &m.guid {
        match &m.value {
            Some(v) => diesel::sql_query(format!("INSERT OR REPLACE INTO match_marks(me, guid, field, value, extra, by, at) VALUES ({}, {}, {}, {}, {}, {}, {})", q(&m.me), q(guid), q(&m.field), q(v), o(&m.extra), q(&m.by), q(&m.at))).execute(conn)?,
            None => diesel::sql_query(format!("DELETE FROM match_marks WHERE me = {} AND guid = {} AND field = {}", q(&m.me), q(guid), q(&m.field))).execute(conn)?,
        };
    } else if let (Some(hash), "experimental") = (&m.hash, m.field.as_str()) {
        diesel::sql_query(format!("UPDATE mtgo_lists SET experimental = {} WHERE me = {} AND hash = {}", (m.value.as_deref() == Some("1")) as i32, q(&m.me), q(hash))).execute(conn)?;
    }
    Ok(())
}

/// Replay the file onto the database, for one player or all; returns the marks applied. A
/// missing file is first written from the experimental tags the database holds, which were
/// set before the file existed.
pub fn apply(conn: &mut SqliteConnection, data: &Path, me: Option<&str>) -> Result<usize, String> {
    let p = path(data);
    if !p.exists() {
        export_list_marks(conn, data)?;
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut n = 0;
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let m: Mark = serde_json::from_str(line).map_err(|e| format!("{} line {}: {e}", p.display(), i + 1))?;
        if me.is_some_and(|me| me != m.me) {
            continue;
        }
        apply_one(conn, &m).map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
}

fn export_list_marks(conn: &mut SqliteConnection, data: &Path) -> Result<(), String> {
    #[derive(QueryableByName)]
    struct L {
        #[diesel(sql_type = diesel::sql_types::Text)]
        me: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        hash: String,
    }
    let rows: Vec<L> = diesel::sql_query("SELECT me, hash FROM mtgo_lists WHERE experimental = 1 AND me IS NOT NULL").load(conn).map_err(|e| e.to_string())?;
    let at = now();
    let text: String = rows.iter().map(|l| serde_json::to_string(&Mark { at: at.clone(), by: "export".into(), me: l.me.clone(), guid: None, hash: Some(l.hash.clone()), field: "experimental".into(), value: Some("1".into()), extra: None }).unwrap() + "\n").collect();
    std::fs::write(path(data), text).map_err(|e| format!("{}: {e}", path(data).display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(dir: &Path) -> SqliteConnection {
        let mut c = crate::db::establish_connection_at(&dir.join("t.db")).unwrap();
        crate::mtgolog::db::ensure_schema(&mut c).unwrap();
        diesel::sql_query("INSERT INTO mtgo_matches(guid, path, me, result, my_game_wins, opp_game_wins, games, record_count, imported_at) VALUES ('g1', 'p', 'bo', 'W', 2, 0, 2, 1, '2026-01-01')").execute(&mut c).unwrap();
        c
    }
    fn marks_of(c: &mut SqliteConnection) -> Vec<(String, String, Option<String>)> {
        #[derive(QueryableByName)]
        struct R {
            #[diesel(sql_type = diesel::sql_types::Text)]
            field: String,
            #[diesel(sql_type = diesel::sql_types::Text)]
            value: String,
            #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
            extra: Option<String>,
        }
        let rows: Vec<R> = diesel::sql_query("SELECT field, value, extra FROM match_marks WHERE me = 'bo' AND guid = 'g1' ORDER BY field").load(c).unwrap();
        rows.into_iter().map(|r| (r.field, r.value, r.extra)).collect()
    }

    #[test]
    fn a_mark_is_appended_applied_cleared_and_replayed() {
        let dir = std::env::temp_dir().join(format!("mtgctl-marks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = db(&dir);
        let m = |field: &str, value: Option<&str>, extra: Option<&str>| Mark { at: now(), by: "bo".into(), me: "bo".into(), guid: Some("g1".into()), hash: None, field: field.into(), value: value.map(String::from), extra: extra.map(String::from) };
        record(&mut c, &dir, &m("opp_deck", Some("UB Hogaak"), Some("Tempo"))).unwrap();
        record(&mut c, &dir, &m("format", Some("Vintage"), None)).unwrap();
        assert_eq!(marks_of(&mut c), vec![("format".into(), "Vintage".into(), None), ("opp_deck".into(), "UB Hogaak".into(), Some("Tempo".into()))]);
        record(&mut c, &dir, &m("format", None, None)).unwrap();
        assert_eq!(marks_of(&mut c).len(), 1);
        assert!(record(&mut c, &dir, &m("result", Some("W"), None)).is_err());
        // a fresh database gets the same state back from the file
        std::fs::remove_file(dir.join("t.db")).unwrap();
        let mut c2 = db(&dir);
        assert_eq!(apply(&mut c2, &dir, Some("bo")).unwrap(), 3);
        assert_eq!(marks_of(&mut c2), vec![("opp_deck".into(), "UB Hogaak".into(), Some("Tempo".into()))]);
        assert_eq!(apply(&mut c2, &dir, Some("someone else")).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
