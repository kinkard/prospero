use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::Context;
use async_trait::async_trait;
use librespot::{
    core::{spotify_id::SpotifyItemType, SpotifyId},
    playback::player::Player,
};
use serenity::all::GuildId;
use songbird::input::{AudioStream, AudioStreamError, AuxMetadata, Compose, Input};
use symphonia::core::io::MediaSource;

mod player;
mod storage;

pub(crate) use storage::Credentials;

pub(crate) struct Manager {
    players: HashMap<GuildId, player::Player>,
    storage: storage::Storage,
}

impl Manager {
    pub(crate) fn new(db_path: &Path) -> Result<Self, anyhow::Error> {
        Ok(Self {
            players: HashMap::new(),
            storage: storage::Storage::new(db_path)?,
        })
    }

    pub(crate) fn save_credentials(&self, credentials: Credentials) -> Result<(), anyhow::Error> {
        // todo: we should validate credentials first
        self.storage.save_credentials(credentials)
    }

    pub(crate) async fn start_player(&mut self, guild_id: GuildId) -> Result<Input, anyhow::Error> {
        if let Some(player) = self.players.get(&guild_id) {
            return Ok(player.audio_input());
        }

        // If there's no player for the guild, retrieve the Spotify credentials from the database
        let credentials = self.storage.load_credentials(guild_id)?;

        // Create a new Player instance with the retrieved Spotify credentials
        let player =
            player::Player::new(credentials.username, Some(credentials.password), None).await?;

        let input = player.audio_input();
        self.players.insert(guild_id, player);
        Ok(input)
    }

    pub(crate) fn stop_player(&mut self, guild_id: GuildId) {
        if let Some(player) = self.players.remove(&guild_id) {
            // explicit drop for better readability
            drop(player);
        }
    }
}

/// Tries to resolve a SpotifyID from a query
/// Returns None if the query doesn't look like a Spotify URI or URL,
/// otherwise returns a SpotifyId or a parsing error
fn resolve(query: &str) -> Option<Result<SpotifyId, anyhow::Error>> {
    let spotify_id = if query.starts_with("spotify:") {
        Some(SpotifyId::from_uri(query).context("Invalid Spotify URI"))
    } else if let Some(remaining) = query.strip_prefix("https://open.spotify.com/") {
        if let Some((item_type, id)) = remaining.split_once('/') {
            let uri = format!("spotify:{}:{}", item_type, id);
            Some(SpotifyId::from_uri(&uri).context("Invalid Spotify URL"))
        } else {
            Some(Err(anyhow::anyhow!("Invalid Spotify URL")))
        }
    } else {
        None
    };

    // Only these types are supported
    let known_types = [
        SpotifyItemType::Track,
        SpotifyItemType::Album,
        SpotifyItemType::Playlist,
    ];
    spotify_id.map(|spotify_id| {
        spotify_id.and_then(|spotify_id| {
            if known_types.contains(&spotify_id.item_type) {
                Ok(spotify_id)
            } else {
                Err(anyhow::anyhow!("Unsupported Spotify item type"))
            }
        })
    })
}

struct Track {
    id: SpotifyId,
    player: Arc<Player>,
}

impl From<Track> for Input {
    fn from(val: Track) -> Self {
        Input::Lazy(Box::new(val))
    }
}

#[async_trait]
impl Compose for Track {
    fn create(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        Err(AudioStreamError::Unsupported)
    }

    async fn create_async(
        &mut self,
    ) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        todo!()
    }

    fn should_create_async(&self) -> bool {
        true
    }

    async fn aux_metadata(&mut self) -> Result<AuxMetadata, AudioStreamError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn query_resolution_test() {
        assert_eq!(
            resolve("spotify:track:6rqhFgbbKwnb9MLmUQDhG6")
                .unwrap()
                .unwrap(),
            SpotifyId::from_uri("spotify:track:6rqhFgbbKwnb9MLmUQDhG6").unwrap()
        );

        assert_eq!(
            resolve("spotify:album:6G9fHYDCoyEErUkHrFYfs4")
                .unwrap()
                .unwrap(),
            SpotifyId::from_uri("spotify:album:6G9fHYDCoyEErUkHrFYfs4").unwrap()
        );

        assert_eq!(
            resolve("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M")
                .unwrap()
                .unwrap(),
            SpotifyId::from_uri("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").unwrap()
        );

        assert_eq!(
            resolve("https://open.spotify.com/track/6rqhFgbbKwnb9MLmUQDhG6")
                .unwrap()
                .unwrap(),
            SpotifyId::from_uri("spotify:track:6rqhFgbbKwnb9MLmUQDhG6").unwrap()
        );

        assert_eq!(
            resolve("https://open.spotify.com/album/6G9fHYDCoyEErUkHrFYfs4")
                .unwrap()
                .unwrap(),
            SpotifyId::from_uri("spotify:album:6G9fHYDCoyEErUkHrFYfs4").unwrap()
        );

        assert_eq!(
            resolve("https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M")
                .unwrap()
                .unwrap(),
            SpotifyId::from_uri("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").unwrap()
        );

        // not matched for Spotify
        assert!(resolve("https://www.youtube.com/watch?v=HnL5lQXuv9M").is_none());
        assert!(resolve("my random raw text query").is_none());
        assert!(resolve("schema:track:6G9fHYDCoyEErUkHrFYfs4").is_none());

        // invalid Spotify URI
        assert!(resolve("spotify:track:invalid").unwrap().is_err());
        assert!(resolve("spotify:album:invalid").unwrap().is_err());
        assert!(resolve("spotify:playlist:invalid").unwrap().is_err());
        assert!(resolve("spotify:unknown_type:37i9dQZF1DXcBWIGoYBM5M")
            .unwrap()
            .is_err());
        // invalid Spotify URL
        assert!(resolve("https://open.spotify.com/track/invalid")
            .unwrap()
            .is_err());
        assert!(resolve("https://open.spotify.com/album/invalid")
            .unwrap()
            .is_err());
        assert!(resolve("https://open.spotify.com/playlist/invalid")
            .unwrap()
            .is_err());
        assert!(
            resolve("https://open.spotify.com/unknown_type/6G9fHYDCoyEErUkHrFYfs4")
                .unwrap()
                .is_err()
        );
    }
}
