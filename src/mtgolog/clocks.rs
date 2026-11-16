//! A player's clock models (web/clocks.html): one JSON document per player, kept in
//! `<data>/shared.db` because `rebuild` recreates the pooled database from the logs.

use diesel::prelude::*;
use diesel::sql_types::Text;
use diesel::sqlite::SqliteConnection;

const DDL: &str = "CREATE TABLE IF NOT EXISTS clock_models (
        player TEXT PRIMARY KEY,
        body TEXT NOT NULL,              -- the page's document, opaque to the server
        updated_at TEXT NOT NULL         -- the document's version: a save names the one it edited
    )";

#[derive(QueryableByName)]
pub struct Stored {
    #[diesel(sql_type = Text)]
    pub body: String,
    #[diesel(sql_type = Text)]
    pub updated_at: String,
}

pub fn load(conn: &mut SqliteConnection, player: &str) -> QueryResult<Option<Stored>> {
    diesel::sql_query(DDL).execute(conn)?;
    diesel::sql_query("SELECT body, updated_at FROM clock_models WHERE player = ?").bind::<Text, _>(player).get_result(conn).optional()
}

/// Replaces the player's document when `base` is the stored version (None: nothing stored yet)
/// and returns the new version; None when the stored document has moved on from `base`.
pub fn save(conn: &mut SqliteConnection, player: &str, body: &str, base: Option<&str>) -> QueryResult<Option<String>> {
    conn.immediate_transaction(|conn| {
        if load(conn, player)?.map(|s| s.updated_at).as_deref() != base {
            return Ok(None);
        }
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        diesel::sql_query("INSERT OR REPLACE INTO clock_models(player, body, updated_at) VALUES (?, ?, ?)").bind::<Text, _>(player).bind::<Text, _>(body).bind::<Text, _>(&at).execute(conn)?;
        Ok(Some(at))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_requires_the_stored_version() {
        let conn = &mut SqliteConnection::establish(":memory:").unwrap();
        assert!(load(conn, "a").unwrap().is_none());
        let v1 = save(conn, "a", "{\"n\":1}", None).unwrap().expect("first save");
        assert_eq!(save(conn, "a", "{\"n\":2}", None).unwrap(), None);
        let v2 = save(conn, "a", "{\"n\":2}", Some(&v1)).unwrap().expect("save on the stored version");
        assert_eq!(save(conn, "a", "{\"n\":3}", Some(&v1)).unwrap(), None);
        let s = load(conn, "a").unwrap().unwrap();
        assert_eq!((s.body.as_str(), s.updated_at), ("{\"n\":2}", v2));
        assert!(load(conn, "b").unwrap().is_none());
    }
}
