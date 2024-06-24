use serenity::{
    client::Context,
    model::{
        id::{ChannelId, GuildId},
        voice::VoiceState,
    },
};
use tracing::info;

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
    let old_guild_channel = old.as_ref().and_then(|o| o.guild_id.zip(o.channel_id));
    let new_guild_channel = new.guild_id.zip(new.channel_id);
    let is_bot = new.user_id == ctx.cache.current_user().id;

    match (old_guild_channel, new_guild_channel) {
        // Joined voice channel from nowhere
        (None, Some((guild_id, channel_id))) => {
            if is_bot {
                bot_joined_vc(ctx, data, guild_id, channel_id).await;
            } else {
                user_joined_vc(ctx, data, guild_id, channel_id).await;
            }
        }
        // Moved from one voice channel to another
        (Some((old_guild_id, old_channel_id)), Some((new_guild_id, new_channel_id)))
            // This check filters out other events like muting, deafening, etc.
            if old_guild_id == new_guild_id && old_channel_id != new_channel_id =>
        {
            if is_bot {
                bot_changed_vc(ctx, data, new_guild_id, old_channel_id, new_channel_id).await;
            } else {
                user_changed_vc(ctx, data, new_guild_id, old_channel_id, new_channel_id).await;
            }
        }
        // Left voice channel
        (Some((guild_id, _channel_id)), None) => {
            if is_bot {
                bot_left_vc(ctx, data, guild_id).await;
            } else {
                user_left_vc(ctx, data, guild_id).await;
            }
        }
        // we don't care
        _ => {}
    }
}

/// Invoked when bot joined a new voice channel
async fn bot_joined_vc(ctx: &Context, _data: &Data, guild_id: GuildId, channel_id: ChannelId) {
    info!(
        "Joined '{}' vc in '{}' guild",
        &ctx.cache.channel(channel_id).unwrap().name,
        &ctx.cache.guild(guild_id).unwrap().name,
    );

    setup_vc(ctx, guild_id).await;
}

/// Invoked when bot changed voice channel either because someone moved it or it moved itself
async fn bot_changed_vc(
    ctx: &Context,
    _data: &Data,
    guild_id: GuildId,
    from: ChannelId,
    to: ChannelId,
) {
    info!(
        "Moved from '{}' vc to '{}' vc in '{}' guild",
        &ctx.cache.channel(from).unwrap().name,
        &ctx.cache.channel(to).unwrap().name,
        &ctx.cache.guild(guild_id).unwrap().name,
    );

    setup_vc(ctx, guild_id).await;
}

/// Invoked when bot left voice channel
async fn bot_left_vc(ctx: &Context, data: &Data, guild_id: GuildId) {
    info!(
        "Left voice chat in '{}' guild",
        &ctx.cache.guild(guild_id).unwrap().name
    );

    data.spotify_resolver.disconnect(guild_id).await;
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

/// Invoked when user changed voice channel
async fn user_changed_vc(
    ctx: &Context,
    _data: &Data,
    guild_id: GuildId,
    _from: ChannelId,
    to: ChannelId,
) {
    // If bot left alone in the voice channel, then we should follow the user to the new channel
    if bot_left_alone(ctx, guild_id) {
        info!("Bot left alone, following the user to the new vc");
        let _ = songbird::get(ctx)
            .await
            .expect("Songbird Voice client placed in at initialisation.")
            .join(guild_id, to)
            .await;
    }
}

/// Invoked when user left a voice channel
async fn user_left_vc(ctx: &Context, _data: &Data, guild_id: GuildId) {
    // Check if bot should leave voice channel when everyone left
    if bot_left_alone(ctx, guild_id) {
        info!("Bot left alone, leaving the vc");
        let _ = songbird::get(ctx)
            .await
            .expect("Songbird Voice client placed in at initialisation.")
            .remove(guild_id)
            .await;
    }
}

async fn setup_vc(ctx: &Context, guild_id: GuildId) {
    if let Some(vc) = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .get(guild_id)
    {
        let mut vc = vc.lock().await;
        // 96k is a default Discord bitrate in guilds without nitro so no need to send more data
        vc.set_bitrate(songbird::driver::Bitrate::BitsPerSecond(96_000));
    }
}

fn bot_left_alone(ctx: &Context, guild_id: GuildId) -> bool {
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
                .is_some_and(|member| !member.user.bot)
        })
        .count()
        == 0
}
