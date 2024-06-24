use std::path::Path;
use std::sync::{Arc, Mutex};

use serenity::all::GuildId;

use crate::{spotify, yt_dlp};

pub(crate) struct Storage(Mutex<rusqlite::Connection>);

impl Storage {
    pub(crate) fn new<P: AsRef<Path>>(db_path: P) -> Result<Arc<Self>, anyhow::Error> {
        let db_conn = rusqlite::Connection::open(db_path)?;
        db_conn.execute(
            "CREATE TABLE IF NOT EXISTS spotify_credentials (
                guild_id INTEGER PRIMARY KEY,
                username TEXT,
                password TEXT
            )",
            (),
        )?;
        db_conn.execute(
            "CREATE TABLE IF NOT EXISTS yt_dlp_queries (
                query TEXT NOT NULL PRIMARY KEY,
                webpage_url TEXT NOT NULL
            )",
            (),
        )?;
        Ok(Arc::new(Self(Mutex::new(db_conn))))
    }
}

impl spotify::CredentialsStorage for Storage {
    fn save(&self, guild_id: GuildId, username: &str, password: &str) -> Result<(), anyhow::Error> {
        self.0.lock().unwrap().execute(
            "INSERT OR REPLACE INTO spotify_credentials (
                guild_id, username, password
            ) VALUES (?1, ?2, ?3)",
            (guild_id.get() as i64, username, password),
        )?;
        Ok(())
    }

    fn load(&self, guild_id: GuildId) -> Option<(String, String)> {
        let db = self.0.lock().unwrap();
        let mut stmt = db
            .prepare(
                "SELECT username, password
                    FROM spotify_credentials
                    WHERE guild_id = ?1",
            )
            .expect("Failed to prepare SELECT statement");
        let mut rows = stmt
            .query_map([guild_id.get() as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .ok()?;
        rows.next().transpose().ok()?
    }
}

impl yt_dlp::QueryCache for Storage {
    fn save(&self, query: &str, webpage_url: &str) -> Result<(), anyhow::Error> {
        self.0.lock().unwrap().execute(
            "INSERT OR REPLACE INTO yt_dlp_queries (
                query, webpage_url
            ) VALUES (?1, ?2)",
            (query, webpage_url),
        )?;
        Ok(())
    }

    fn load(&self, query: &str) -> Option<String> {
        let db = self.0.lock().unwrap();
        let mut stmt = db
            .prepare(
                "SELECT webpage_url
                    FROM yt_dlp_queries
                    WHERE query = ?1",
            )
            .expect("Failed to prepare SELECT statement");
        let mut rows = stmt.query_map([query], |row| row.get(0)).ok()?;
        rows.next().transpose().ok()?
    }

    fn load_all(&self) -> Vec<String> {
        let db = self.0.lock().unwrap();
        let mut stmt = db
            .prepare("SELECT DISTINCT webpage_url FROM yt_dlp_queries")
            .expect("Failed to prepare SELECT statement");
        let rows = stmt.query_map([], |row| row.get(0)).ok();
        rows.into_iter().flatten().flatten().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn spotify_credentials_storage() {
        let storage: Arc<dyn spotify::CredentialsStorage> = Storage::new(":memory:").unwrap();

        let first_guild_id = GuildId::new(101);
        assert!(storage
            .save(first_guild_id, "my username", "my password")
            .is_ok());
        assert_eq!(
            storage.load(first_guild_id),
            Some(("my username".into(), "my password".into()))
        );

        // store same Spotify username for another guild
        let guild_id = GuildId::new(202);
        assert!(storage
            .save(guild_id, "my username", "another password")
            .is_ok());
        assert_eq!(
            storage.load(guild_id),
            Some(("my username".into(), "another password".into()))
        );

        // update the username and password
        assert!(storage
            .save(guild_id, "another username", "third password")
            .is_ok());
        assert_eq!(
            storage.load(guild_id),
            Some(("another username".into(), "third password".into()))
        );

        // First guild should not be affected
        assert_eq!(
            storage.load(first_guild_id),
            Some(("my username".into(), "my password".into()))
        );

        // Non-existing guild
        assert_eq!(storage.load(GuildId::new(303)), None);
    }

    #[test]
    fn yt_dlp_query_cache() {
        let storage: Arc<dyn yt_dlp::QueryCache> = Storage::new(":memory:").unwrap();

        assert_eq!(storage.load("query"), None);
        assert_eq!(storage.load_all(), Vec::<String>::new());

        assert!(storage.save("query", "webpage_url").is_ok());
        assert_eq!(storage.load("query"), Some("webpage_url".into()));
        assert_eq!(storage.load_all(), vec!["webpage_url"]);

        assert_eq!(storage.load("another query"), None);
        assert!(storage.save("another query", "another url").is_ok());
        assert_eq!(storage.load("another query"), Some("another url".into()));
        assert_eq!(storage.load_all(), vec!["webpage_url", "another url"]);

        // Update the query
        assert!(storage.save("query", "another url").is_ok());
        assert_eq!(storage.load("query"), Some("another url".into()));
        // Duplicate entries should be merged
        assert_eq!(storage.load_all(), vec!["another url"]);
    }
}
