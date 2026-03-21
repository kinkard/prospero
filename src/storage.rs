use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::yt_dlp;

pub(crate) struct Storage(Mutex<rusqlite::Connection>);

impl Storage {
    pub(crate) fn new<P: AsRef<Path>>(db_path: P) -> Result<Arc<Self>, anyhow::Error> {
        let db_conn = rusqlite::Connection::open(db_path)?;
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
