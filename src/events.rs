use serenity::{
    async_trait,
    client::{Context, EventHandler},
    model::{id::GuildId, voice::VoiceState},
};

use crate::{player, voice};

pub(crate) struct Handler;

#[async_trait]
impl EventHandler for Handler {
    // Use `cache_ready()` instead of `ready()` to have an ability to do stuff that requires cache
    async fn cache_ready(&self, ctx: Context, guilds: Vec<GuildId>) {
        let self_user_id = ctx.cache.current_user_field(|current_user| {
            println!("{} is connected!", current_user.name);
            current_user.id
        });

        // If bot is already in some voice channel when launched we should emit event to sync our state
        // as unfortunately `songbird` doesn't handle this case for us.
        let channels_to_join = guilds
            .into_iter()
            .filter_map(|id| ctx.cache.guild(id))
            .filter_map(|guild| {
                guild
                    .voice_states
                    .get(&self_user_id)
                    .and_then(|voice_state| voice_state.channel_id)
                    .map(|channel_id| (guild.id, channel_id))
            });
        for (guild_id, channel_id) in channels_to_join {
            let _vc_handler = songbird::get(&ctx)
                .await
                .expect("Songbird Voice client placed in at initialisation.")
                .join(guild_id, channel_id)
                .await;
        }
    }

    async fn voice_state_update(&self, ctx: Context, _old: Option<VoiceState>, new: VoiceState) {
        if new.user_id == ctx.cache.current_user_id() {
            let player = player::get(&ctx)
                .await
                .expect("Spotify Player should be placed in at initialisation");

            if let (Some(guild_id), Some(channel_id)) = (new.guild_id, new.channel_id) {
                // Bot joined or was moved into a new voice channel, need to connect to the voice.
                // But as we deal with the events, it might happen that we already left vc at the moment
                // of processing this event, so we should check that `vc` exists in songbird.
                if let Some(vc) = songbird::get(&ctx)
                    .await
                    .expect("Songbird Voice client placed in at initialisation.")
                    .get(guild_id)
                {
                    let mut vc = vc.lock().await;

                    voice::Receiver::subscribe(&mut vc);

                    // 96k is a default Discord bitrate in guilds without nitro and we pull Spotify with 96k
                    vc.set_bitrate(songbird::driver::Bitrate::BitsPerSecond(96_000));
                    vc.play_only_source(player.audio_source());

                    player.play();
                    println!("Joined {channel_id}");
                }
            } else {
                // Bot left voice channel, let's stop player
                player.pause();
                println!("Left voice chat");
            }
        }
    }
}
