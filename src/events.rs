use serenity::{
    async_trait,
    client::{Context, EventHandler},
    model::{id::GuildId, voice::VoiceState},
};
use tracing::{info, warn};

pub(crate) struct Handler;

#[async_trait]
impl EventHandler for Handler {
    // Use `cache_ready()` instead of `ready()` to have an ability to do stuff that requires cache
    async fn cache_ready(&self, ctx: Context, guilds: Vec<GuildId>) {
        let self_user_id = {
            let self_user = ctx.cache.current_user();
            info!("{} is connected!", self_user.name);
            self_user.id
        };

        // Grab bot's current state on the moment of launching
        let bot_guilds = guilds
            .into_iter()
            .filter_map(|id| ctx.cache.guild(id))
            .map(|guild| {
                let channel_id = guild
                    .voice_states
                    .get(&self_user_id)
                    .and_then(|voice_state| voice_state.channel_id);
                (guild.id, channel_id)
            });

        for (guild_id, channel_id) in bot_guilds {
            // If bot is already in some voice channel when launched we should emit event to sync our state
            // as unfortunately `songbird` doesn't handle this case for us.
            // see https://github.com/serenity-rs/songbird/issues/113
            if let Some(channel_id) = channel_id {
                let _vc_handler = songbird::get(&ctx)
                    .await
                    .expect("Songbird Voice client placed in at initialisation.")
                    .join(guild_id, channel_id)
                    .await;
            }
        }
    }

    async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        if new.user_id == ctx.cache.current_user().id {
            if let (Some(guild_id), Some(channel_id)) = (new.guild_id, new.channel_id) {
                // Bot joined or was moved into a new voice channel, need to connect to the voice.
                // But as we deal with the events, it might happen that we already left vc at the moment
                // of processing this event, so we should check that `vc` exists in songbird.
                let Some(vc) = songbird::get(&ctx)
                    .await
                    .expect("Songbird Voice client placed in at initialisation.")
                    .get(guild_id)
                else {
                    warn!("Tried to join voice channel , but it doesn't exist in songbird");
                    return;
                };

                info!(
                    "Joined '{}' vc in '{}' guild",
                    &ctx.cache.guild(guild_id).unwrap().name,
                    &ctx.cache.channel(channel_id).unwrap().name
                );

                // 96k is a default Discord bitrate in guilds without nitro and we pull Spotify with 96k
                let mut vc = vc.lock().await;
                vc.set_bitrate(songbird::driver::Bitrate::BitsPerSecond(96_000));
            } else {
                // Bot left voice channel, let's stop player
                let guild_id = old
                    .expect("Old vc state should be initialized when leaving the channel")
                    .guild_id
                    .expect("Old vc state should contain guild_id when leaving the channel");
                let guild_name = &ctx.cache.guild(guild_id).unwrap().name;
                info!("Left voice chat in '{guild_name}' guild");
            }
        } else {
            // Check if bot should leave voice channel when everyone left
            let Some(guild_id) = old.as_ref().and_then(|old| old.guild_id) else {
                return;
            };

            let bot_left_alone = {
                let guild = ctx.cache.guild(guild_id).unwrap();

                let bot_channel = guild
                    .voice_states
                    .get(&ctx.cache.current_user().id)
                    .and_then(|voice_state| voice_state.channel_id);

                guild
                    .voice_states
                    .values()
                    .filter(|voice_state| voice_state.channel_id == bot_channel)
                    // Ignore non-members and bots
                    .filter(|voice_state| {
                        voice_state
                            .member
                            .as_ref()
                            .map(|member| !member.user.bot)
                            .unwrap_or(false)
                    })
                    .count()
                    == 0
            };

            if bot_left_alone {
                let _ = songbird::get(&ctx)
                    .await
                    .expect("Songbird Voice client placed in at initialisation.")
                    .remove(guild_id)
                    .await;
            }
        }
    }
}
