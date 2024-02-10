use std::path::Path;

use serenity::all::GuildId;

#[derive(Debug, PartialEq)]
pub(crate) struct Credentials {
    /// Guild where these credentials will be used
    pub(crate) guild_id: GuildId,
    pub(crate) username: String,
    pub(crate) password: String,
}

pub(crate) struct Storage(rusqlite::Connection);

impl Storage {
    pub(crate) fn new<P: AsRef<Path>>(db_path: P) -> Result<Self, anyhow::Error> {
        let db_conn = rusqlite::Connection::open(db_path)?;
        db_conn.execute(
            "CREATE TABLE IF NOT EXISTS spotify_credentials (
                guild_id INTEGER PRIMARY KEY,
                username TEXT,
                password TEXT
            )",
            (),
        )?;
        Ok(Self(db_conn))
    }

    pub(crate) fn save_credentials(&self, credentials: Credentials) -> Result<(), anyhow::Error> {
        self.0.execute(
            "INSERT OR REPLACE INTO spotify_credentials (
                guild_id, username, password
            ) VALUES (?1, ?2, ?3)",
            (
                credentials.guild_id.get() as i64,
                credentials.username,
                credentials.password,
            ),
        )?;
        Ok(())
    }

    pub(crate) fn load_credentials(&self, guild_id: GuildId) -> Result<Credentials, anyhow::Error> {
        let mut stmt = self
            .0
            .prepare(
                "SELECT username, password 
                    FROM spotify_credentials
                    WHERE guild_id = ?1",
            )
            .unwrap();
        let mut rows = stmt.query([guild_id.get() as i64])?;
        let row = rows.next()?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Ok(Credentials {
            guild_id,
            username: row.get(0)?,
            password: row.get(1)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn storage_test() {
        let storage = Storage::new(":memory:").unwrap();

        let guild_id = GuildId::new(101);
        assert!(storage
            .save_credentials(Credentials {
                guild_id,
                username: "my username".into(),
                password: "my password".into(),
            })
            .is_ok());
        assert_eq!(
            storage.load_credentials(guild_id).unwrap(),
            Credentials {
                guild_id,
                username: "my username".into(),
                password: "my password".into(),
            }
        );

        // store same Spotify username for another guild
        let guild_id = GuildId::new(202);
        assert!(storage
            .save_credentials(Credentials {
                guild_id,
                username: "my username".into(),
                password: "another password".into(),
            })
            .is_ok());
        assert_eq!(
            storage.load_credentials(guild_id).unwrap(),
            Credentials {
                guild_id,
                username: "my username".into(),
                password: "another password".into(),
            }
        );

        // update the password
        assert!(storage
            .save_credentials(Credentials {
                guild_id,
                username: "another username".into(),
                password: "another password".into(),
            })
            .is_ok());
        assert_eq!(
            storage.load_credentials(guild_id).unwrap(),
            Credentials {
                guild_id,
                username: "another username".into(),
                password: "another password".into(),
            }
        );
    }
}
