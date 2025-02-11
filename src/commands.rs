use std::fmt::Write;

use poise::CreateReply;
use serenity::builder::{CreateEmbed, CreateMessage};
use smallvec::{smallvec, SmallVec};
use songbird::{input::Input, tracks::Track};
use tracing::info;

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
    // Send a ephemeral reply to reduce spam
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

    #[cfg(feature = "spotify")]
    let spotify_tracks: Option<SmallVec<[(track_info::Metadata, Input); 1]>> = ctx
        .data()
        .spotify_resolver
        .resolve(guild_id, &query)
        .await
        .map(|tracks| {
            tracks
                .into_iter()
                .map(|track| (track.metadata().clone(), track.into()))
                .collect()
        });
    #[cfg(not(feature = "spotify"))]
    let spotify_tracks: Option<SmallVec<[(track_info::Metadata, Input); 1]>> = None;

    let resolved_items: SmallVec<[(track_info::Metadata, Input); 1]> =
        if let Some(tracks) = spotify_tracks {
            if tracks.is_empty() {
                ctx.reply(format!(
                    "Invalid Spotify query '{query}'. Please try something else"
                ))
                .await?;
                return Ok(());
            }
            tracks
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

    let vc = vc.await??;
    let mut vc = vc.lock().await;
    for (metadata, input) in resolved_items {
        // Reduce volume to 50% to avoid ear damage for new users
        let track = Track::from(input).volume(0.5);
        let track_handle = vc.enqueue(track).await;

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

/// Connect Spotify account to be used by bot.
/// https://www.spotify.com/us/account/set-device-password/
#[poise::command(guild_only, slash_command)]
#[cfg(feature = "spotify")]
pub(crate) async fn connect_spotify(
    ctx: Context<'_>,
    username: String,
    password: String,
) -> Result<(), anyhow::Error> {
    let guild_id = ctx.guild().unwrap().id;

    let result = ctx
        .data()
        .spotify_resolver
        .connect(guild_id, username, password)
        .await;
    let reply = if let Err(err) = result {
        format!("Failed to connect Spotify account: {err:#}.")
    } else {
        "Spotify account connected successfully.".into()
    };

    // Show reply only to user who invoked the command to avoid credentials leakage
    ctx.send(CreateReply::default().content(reply).ephemeral(true))
        .await?;
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
    let mut remaining = 0;
    for track in &mut tracks {
        let typemap = track.typemap().read().await;
        let description = typemap.get::<track_info::TrackInfoKey>().unwrap();

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
