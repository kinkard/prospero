use std::{collections::HashMap, path::Path, sync::Arc};

use serenity::{all::GuildId, client::Context, prelude::TypeMapKey};
use songbird::input::Input;
use tokio::sync::Mutex;

use crate::commands;

mod player;
mod storage;

pub(crate) use storage::Credentials;

/// Key to store spotify::Manager in the serenity context
pub(crate) struct ManagerKey;

impl TypeMapKey for ManagerKey {
    type Value = Arc<Mutex<Manager>>;
}

pub(crate) async fn get_manager(ctx: &Context) -> Option<Arc<Mutex<Manager>>> {
    let data = ctx.data.read().await;
    data.get::<ManagerKey>().cloned()
}

pub(crate) struct Manager {
    players: HashMap<GuildId, player::Player>,
    storage: storage::Storage,
}

impl Manager {
    pub(crate) fn new(db_path: &Path) -> Result<Self, commands::Error> {
        Ok(Self {
            players: HashMap::new(),
            storage: storage::Storage::new(db_path)?,
        })
    }

    pub(crate) fn save_credentials(&self, credentials: Credentials) -> Result<(), commands::Error> {
        // todo: we should validate credentials first
        self.storage.save_credentials(credentials)
    }

    pub(crate) async fn start_player(
        &mut self,
        guild_id: GuildId,
    ) -> Result<Input, commands::Error> {
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
