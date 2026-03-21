use std::fmt::Write;
use std::sync::Arc;

use poise::CreateReply;
use serenity::builder::{CreateEmbed, CreateMessage};
use songbird::{input::Input, tracks::Track};
use tracing::info;

use crate::{Context, DEFAULT_VOLUME, track_info, volume_to_amplitude};

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
    ctx.send(CreateReply::default().content("Pong!").ephemeral(true))
        .await?;
    Ok(())
}

/// Play a song from a URL or search query
#[poise::command(guild_only, slash_command)]
pub(crate) async fn play(ctx: Context<'_>, query: String) -> Result<(), anyhow::Error> {
    info!("{} requested to play '{query}'", ctx.author().name);

    let Some(channel_id) = get_author_vc(&ctx) else {
        ctx.reply("You should be in a voice channel if you want me to play for you")
            .await?;
        return Ok(());
    };

    let songbird = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.");

    let guild_id = ctx.guild().unwrap().id;

    // Do it in a separate task as joining a voice channel can take some time
    let vc = tokio::spawn(async move {
        match songbird.get(guild_id) {
            // todo: check if bot is in the same channel as the user
            Some(vc) => Ok(vc),
            None => songbird.join(guild_id, channel_id).await,
        }
    });

    let _ = ctx.reply(format!("Processing {query}...")).await;

    let (metadata, input): (track_info::Metadata, Input) =
        if let Some(yt_dlp) = ctx.data().yt_dlp_resolver.resolve(&query).await {
            (yt_dlp.metadata().clone(), yt_dlp.into())
        } else {
            ctx.reply(format!(
                "Found nothing for '{query}'. Please try something else"
            ))
            .await?;
            return Ok(());
        };

    let vc = vc.await??;
    let mut vc = vc.lock().await;

    // Attach description to the track handle so we can display each entry in the queue
    let track = Track::new_with_data(
        input,
        Arc::new(track_info::TrackInfo::new(
            metadata,
            ctx.author().name.clone(),
        )),
    )
    .volume(volume_to_amplitude(
        ctx.data()
            .guild_volumes
            .read()
            .unwrap()
            .get(&guild_id)
            .copied()
            .unwrap_or(DEFAULT_VOLUME),
    ));
    let _ = vc.enqueue(track).await;

    let queue_info = form_currently_played(&vc.queue().current_queue()).await;
    drop(vc);

    // fetching track info from yt-dlp may take some time (youtube seems to slow down such requests),
    // so instead of replying we send a message.
    ctx.channel_id()
        .send_message(
            ctx.serenity_context(),
            CreateMessage::default().embed(queue_info),
        )
        .await?;

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

    // queue().skip() doesn't update the queue immediately, so take it *before* skipping
    let queue_info =
        form_currently_played(vc.queue().current_queue().get(1..).unwrap_or_default()).await;
    let _ = vc.queue().skip();
    drop(vc);

    ctx.send(CreateReply::default().embed(queue_info)).await?;
    Ok(())
}

/// Set playback volume (0-10, default 5)
#[poise::command(guild_only, slash_command)]
pub(crate) async fn volume(
    ctx: Context<'_>,
    #[description = "Volume level (0-10)"]
    #[min = 0]
    #[max = 10]
    level: u8,
) -> Result<(), anyhow::Error> {
    let guild_id = ctx.guild().unwrap().id;

    ctx.data()
        .guild_volumes
        .write()
        .unwrap()
        .insert(guild_id, level);

    let amplitude = volume_to_amplitude(level);

    // Apply to all currently queued tracks
    let songbird = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.");

    if let Some(vc) = songbird.get(guild_id) {
        let vc = vc.lock().await;
        for track in vc.queue().current_queue() {
            let _ = track.set_volume(amplitude);
        }
    }

    ctx.reply(format!("Volume set to {level}/10")).await?;
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
        track
            .data::<track_info::TrackInfo>()
            .build_embed()
            .title("Now Playing")
    } else {
        CreateEmbed::default().title("Nothing to play! Add new tracks with `/play` command")
    };

    // and then add all the other tracks to the description
    let mut next_str = String::new();
    let mut remaining = 0;
    for track in &mut tracks {
        let description = track.data::<track_info::TrackInfo>();

        let prev_size = next_str.len();
        let _ = writeln!(next_str, "- {description}");

        // Discord supports up to 1024 characters in embed body.
        // Keep 16 characters space for the "... and N more" message
        if next_str.len() > 1024 - 16 {
            next_str.truncate(prev_size);
            remaining += 1; // don't forget about the current track that we just discarded
            break;
        }
    }
    remaining += tracks.count();
    if remaining > 0 {
        let _ = writeln!(next_str, "... and {remaining} more");
    }

    if !next_str.is_empty() {
        embed.field("Next:", next_str, false)
    } else {
        embed
    }
}
