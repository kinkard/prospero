use poise::CreateReply;
use serenity::builder::CreateEmbed;
use songbird::input::{Compose, YoutubeDl};
use tracing::{info, warn};

use crate::{http_client, track_info};

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, (), Error>;

fn get_author_vc(ctx: &Context<'_>) -> Option<serenity::model::id::ChannelId> {
    ctx.guild()?
        .voice_states
        .get(&ctx.author().id)
        .and_then(|voice_state| voice_state.channel_id)
}

/// Join my current voice channel
#[poise::command(guild_only, slash_command)]
pub(crate) async fn join(ctx: Context<'_>) -> Result<(), Error> {
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
pub(crate) async fn leave(ctx: Context<'_>) -> Result<(), Error> {
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
pub(crate) async fn ping(ctx: Context<'_>) -> Result<(), Error> {
    ctx.reply("Pong!").await?;
    Ok(())
}

/// Play a song from a URL
#[poise::command(guild_only, slash_command)]
pub(crate) async fn play(ctx: Context<'_>, url: String) -> Result<(), Error> {
    info!("{} requested to play {url}", ctx.author().name);
    let guild_id = ctx.guild().unwrap().id;

    if !url.starts_with("http") {
        ctx.reply("Must provide a valid URL that starts with `http`")
            .await?;
        return Ok(());
    }

    let http_client = http_client::get(ctx.serenity_context())
        .await
        .expect("HttpClient should be inserted in at initialisation");
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

    let mut src = YoutubeDl::new(http_client, url.clone());
    let metadata = match src.aux_metadata().await {
        Ok(meta) => Some(meta),
        Err(err) => {
            warn!("Failed to get metadata for {url}: {err}");
            None
        }
    };

    let mut vc = vc.lock().await;
    let track_handle = vc.enqueue(src.into()).await;

    // Attach description to the track handle so we can display each entry in the queue
    track_handle
        .typemap()
        .write()
        .await
        .insert::<track_info::TrackInfoKey>(track_info::TrackInfo::new(
            url,
            metadata,
            ctx.author().name.clone(),
        ));

    let queue_info = form_currently_played(vc.queue().current_queue()).await;
    ctx.send(CreateReply::default().embed(queue_info)).await?;

    Ok(())
}

/// Skip the current song
#[poise::command(guild_only, slash_command)]
pub(crate) async fn skip(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild().unwrap().id;
    let songbird = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.");

    let Some(vc) = songbird.get(guild_id) else {
        ctx.reply("I'm not in a voice channel").await?;
        return Ok(());
    };

    let vc = vc.lock().await;

    // Unfortunately, `queue().skip()` doesn't update queue immidiately, so skip(1) is required
    // here to show the correct queue info.
    // And instead of relying on this behavior we form info *before* the skipping the track
    let queue_info = form_currently_played(vc.queue().current_queue().into_iter().skip(1)).await;
    ctx.send(CreateReply::default().embed(queue_info)).await?;

    let _ = vc.queue().skip();

    Ok(())
}

/// Stop playing and clear the queue
#[poise::command(guild_only, slash_command)]
pub(crate) async fn stop(ctx: Context<'_>) -> Result<(), Error> {
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

async fn form_currently_played<It>(tracks: It) -> CreateEmbed
where
    It: IntoIterator<Item = songbird::tracks::TrackHandle>,
{
    let mut tracks = tracks.into_iter();

    // Use the first track in the queue to form the embed
    let embed = if let Some(track) = tracks.next() {
        let typemap = track.typemap().read().await;
        typemap
            .get::<track_info::TrackInfoKey>()
            .unwrap()
            .into_embed()
            .title("Now Playing")
    } else {
        CreateEmbed::default().title("Nothing to play! Add new tracks with `/play` command")
    };

    // and then add all the other tracks to the description
    let mut next_str = String::new();
    while let Some(track) = tracks.next() {
        let typemap = track.typemap().read().await;
        let description = typemap.get::<track_info::TrackInfoKey>().unwrap();

        use std::fmt::Write;
        let _ = write!(next_str, "- {description}\n");
    }
    if !next_str.is_empty() {
        embed.field("Next:", next_str, false)
    } else {
        embed
    }
}
