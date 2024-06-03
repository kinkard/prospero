use poise::CreateReply;
use serenity::builder::{CreateEmbed, CreateMessage};
use songbird::input::{Compose, Input};
use tracing::{info, warn};

use crate::{track_info, Context};

fn get_author_vc(ctx: &Context<'_>) -> Option<serenity::model::id::ChannelId> {
    ctx.guild()?
        .voice_states
        .get(&ctx.author().id)
        .and_then(|voice_state| voice_state.channel_id)
}

/// Join my current voice channel
#[poise::command(guild_only, slash_command)]
pub(crate) async fn join(ctx: Context<'_>) -> Result<(), anyhow::Error> {
    let guild_id = ctx.guild().unwrap().id;
    let Some(channel_id) = get_author_vc(&ctx) else {
        ctx.reply("You should be in a voice channel to invite me")
            .await?;
        return Ok(());
    };

    let _vc_handler = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .join(guild_id, channel_id)
        .await;

    ctx.reply("Joined voice channel").await?;
    Ok(())
}

/// Leave voice channel
#[poise::command(guild_only, slash_command)]
pub(crate) async fn leave(ctx: Context<'_>) -> Result<(), anyhow::Error> {
    let guild_id = ctx.guild().unwrap().id;

    songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .remove(guild_id)
        .await?;

    ctx.reply("Left voice channel").await?;
    Ok(())
}

/// Ask bot to say "Pong!"
#[poise::command(slash_command)]
pub(crate) async fn ping(ctx: Context<'_>) -> Result<(), anyhow::Error> {
    ctx.reply("Pong!").await?;
    Ok(())
}

/// Play a song from a URL or search query
#[poise::command(guild_only, slash_command)]
pub(crate) async fn play(ctx: Context<'_>, query: String) -> Result<(), anyhow::Error> {
    info!("{} requested to play '{query}'", ctx.author().name);
    let guild_id = ctx.guild().unwrap().id;

    let songbird = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.");

    let vc = match songbird.get(guild_id) {
        Some(vc) => vc,
        None => {
            let Some(channel_id) = get_author_vc(&ctx) else {
                ctx.reply("You should be in a voice channel to invite me")
                    .await?;
                return Ok(());
            };
            songbird.join(guild_id, channel_id).await?
        }
    };

    let (input, metadata): (Input, _) =
        if let Some(mut track) = ctx.data().spotify_player.resolve(&query).await {
            let metadata = track.aux_metadata().await.ok();
            (track.into(), metadata)
        } else if let Some(mut yt_dlp) = ctx.data().yt_dlp_resolver.resolve(&query).await {
            let metadata = yt_dlp.aux_metadata().await.ok();
            (yt_dlp.into(), metadata)
        } else {
            ctx.reply(format!(
                "Found nothing for '{query}'. Please try something else"
            ))
            .await?;
            return Ok(());
        };

    let mut vc = vc.lock().await;
    let track_handle = vc.enqueue(input.into()).await;

    // Attach description to the track handle so we can display each entry in the queue
    track_handle
        .typemap()
        .write()
        .await
        .insert::<track_info::TrackInfoKey>(track_info::TrackInfo::new(
            query,
            metadata,
            ctx.author().name.clone(),
        ));

    let queue_info = form_currently_played(&vc.queue().current_queue()).await;
    if let Err(err) = ctx
        .send(CreateReply::default().embed(queue_info.clone()))
        .await
    {
        warn!("Failed to reply: {err}. Falling back to sending a message");

        // Fallback to sending a message if embed failed
        ctx.channel_id()
            .send_message(
                ctx.serenity_context(),
                CreateMessage::default().embed(queue_info),
            )
            .await?;
    }

    Ok(())
}

/// Skip the current song
#[poise::command(guild_only, slash_command)]
pub(crate) async fn skip(ctx: Context<'_>) -> Result<(), anyhow::Error> {
    let guild_id = ctx.guild().unwrap().id;
    let songbird = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.");

    let Some(vc) = songbird.get(guild_id) else {
        ctx.reply("I'm not in a voice channel").await?;
        return Ok(());
    };

    let vc = vc.lock().await;

    // Unfortunately, `queue().skip()` doesn't update queue immidiately, so we take the queue *before*
    // and form the embed with tracks after the current one via `get(1..)`
    let queue_info =
        form_currently_played(vc.queue().current_queue().get(1..).unwrap_or_default()).await;
    ctx.send(CreateReply::default().embed(queue_info)).await?;

    let _ = vc.queue().skip();

    Ok(())
}

/// Stop playing and clear the queue
#[poise::command(guild_only, slash_command)]
pub(crate) async fn stop(ctx: Context<'_>) -> Result<(), anyhow::Error> {
    let guild_id = ctx.guild().unwrap().id;
    let songbird = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.");

    if let Some(vc) = songbird.get(guild_id) {
        let vc = vc.lock().await;
        vc.queue().stop();
    };

    ctx.reply("Stopped playing and cleared the queue").await?;
    Ok(())
}

async fn form_currently_played(tracks: &[songbird::tracks::TrackHandle]) -> CreateEmbed {
    let mut tracks = tracks.into_iter();

    // Use the first track in the queue to form the embed
    let embed = if let Some(track) = tracks.next() {
        let typemap = track.typemap().read().await;
        typemap
            .get::<track_info::TrackInfoKey>()
            .unwrap()
            .build_embed()
            .title("Now Playing")
    } else {
        CreateEmbed::default().title("Nothing to play! Add new tracks with `/play` command")
    };

    // and then add all the other tracks to the description
    let mut next_str = String::new();
    for track in tracks {
        let typemap = track.typemap().read().await;
        let description = typemap.get::<track_info::TrackInfoKey>().unwrap();

        use std::fmt::Write;
        let _ = writeln!(next_str, "- {description}");
    }
    if !next_str.is_empty() {
        embed.field("Next:", next_str, false)
    } else {
        embed
    }
}
