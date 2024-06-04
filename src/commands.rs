use std::fmt::Write;

use poise::CreateReply;
use serenity::builder::{CreateEmbed, CreateMessage};
use smallvec::{smallvec, SmallVec};
use songbird::input::Input;
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

    let resolved_items: SmallVec<[(track_info::Metadata, Input); 1]> =
        if let Some(tracks) = ctx.data().spotify_player.resolve(&query).await {
            if tracks.is_empty() {
                ctx.reply(format!(
                    "Invalid Spotify query '{query}'. Please try something else"
                ))
                .await?;
                return Ok(());
            }
            tracks
                .into_iter()
                .map(|track| (track.metadata().clone(), track.into()))
                .collect()
        } else if let Some(podcast) = ctx.data().radio_t_resolver.resolve(&query).await {
            smallvec![(podcast.metadata().clone(), podcast.into())]
        } else if let Some(yt_dlp) = ctx.data().yt_dlp_resolver.resolve(&query).await {
            smallvec![(yt_dlp.metadata().clone(), yt_dlp.into())]
        } else {
            ctx.reply(format!(
                "Found nothing for '{query}'. Please try something else"
            ))
            .await?;
            return Ok(());
        };

    let mut vc = vc.lock().await;
    for (metadata, input) in resolved_items {
        let track_handle = vc.enqueue(input.into()).await;

        // Attach description to the track handle so we can display each entry in the queue
        track_handle
            .typemap()
            .write()
            .await
            .insert::<track_info::TrackInfoKey>(track_info::TrackInfo::new(
                metadata,
                ctx.author().name.clone(),
            ));
    }
    let queue_info = form_currently_played(&vc.queue().current_queue()).await;
    drop(vc);

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
    let _ = vc.queue().skip();
    drop(vc);

    ctx.send(CreateReply::default().embed(queue_info)).await?;
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
    let mut tracks = tracks.iter();

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

        let size = next_str.len();
        let _ = writeln!(next_str, "- {description}");

        // Discord supports up to 1024 characters in embed body
        if next_str.len() > 1024 - 5 {
            next_str.truncate(size);
            next_str.push_str("- ...");
            break;
        }
    }
    if !next_str.is_empty() {
        embed.field("Next:", next_str, false)
    } else {
        embed
    }
}
