use serenity::{
    client::Context,
    model::{
        id::{ChannelId, GuildId},
        voice::VoiceState,
    },
};
use tracing::{info, warn};

use crate::Data;

/// Invoked once, quickly after bot started, when the cache has received and inserted all data
/// from guilds. Can be considered as an entry point for all preparations.
pub(crate) async fn cache_ready(ctx: &Context, data: &Data, guilds: &[GuildId]) {
    let self_user_id = {
        let self_user = ctx.cache.current_user();
        info!("{} is connected!", self_user.name);
        self_user.id
    };

    // Grab bot's current state on the moment of launching
    let bot_guilds = guilds
        .iter()
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
            let _vc_handler = songbird::get(ctx)
                .await
                .expect("Songbird Voice client placed in at initialisation.")
                .join(guild_id, channel_id)
                .await;
        }
    }

    data.yt_dlp_resolver.load_cache().await;
}

/// Invoked when a user joins, leaves or moves to a voice channel.
pub(crate) async fn voice_state_update(
    ctx: &Context,
    data: &Data,
    old: &Option<VoiceState>,
    new: &VoiceState,
) {
    if let (Some(guild_id), Some(channel_id)) = (new.guild_id, new.channel_id) {
        if new.user_id == ctx.cache.current_user().id {
            bot_joined_vc(ctx, data, guild_id, channel_id).await;
        } else {
            user_joined_vc(ctx, data, guild_id, channel_id).await;
        }
    } else if let Some(guild_id) = old.as_ref().and_then(|old| old.guild_id) {
        if new.user_id == ctx.cache.current_user().id {
            bot_left_vc(ctx, data, guild_id).await;
        } else {
            user_left_vc(ctx, data, guild_id).await;
        }
    }
}

/// Invoked when bot joined or was moved into a new voice channel
async fn bot_joined_vc(ctx: &Context, _data: &Data, guild_id: GuildId, channel_id: ChannelId) {
    // Bot joined or was moved into a new voice channel, need to setup the vc.
    // But as we deal with the events, it might happen that we already left vc at the moment
    // of processing this event, so we should check that `vc` exists in songbird.
    let Some(vc) = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .get(guild_id)
    else {
        warn!("Tried to join voice channel, but it doesn't exist in songbird");
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
}

/// Invoked when bot left voice channel
async fn bot_left_vc(ctx: &Context, data: &Data, guild_id: GuildId) {
    info!(
        "Left voice chat in '{}' guild",
        &ctx.cache.guild(guild_id).unwrap().name
    );

    // Let's save yt_dlp_resolver state
    data.yt_dlp_resolver.save_cache().await;
}

/// Invoked when user joined a voice channel
async fn user_joined_vc(ctx: &Context, data: &Data, guild_id: GuildId, _channel_id: ChannelId) {
    let users_in_vc = {
        let guild = ctx.cache.guild(guild_id).unwrap();
        guild.voice_states.len()
    };

    // the first person joined a vc, update yt_dlp_resolver
    if users_in_vc == 1 {
        info!("First person joined a vc, updating yt_dlp_resolver's cache");
        data.yt_dlp_resolver.update_cache().await;
    }
}

/// Invoked when user left a voice channel
async fn user_left_vc(ctx: &Context, _data: &Data, guild_id: GuildId) {
    // Check if bot should leave voice channel when everyone left
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
        let _ = songbird::get(ctx)
            .await
            .expect("Songbird Voice client placed in at initialisation.")
            .remove(guild_id)
            .await;
    }
}
