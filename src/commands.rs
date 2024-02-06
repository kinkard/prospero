use poise::CreateReply;
use serenity::builder::CreateEmbed;
use songbird::{
    input::{AuxMetadata, Compose, YoutubeDl},
    tracks::TrackQueue,
};
use tracing::{info, warn};

use crate::http_client;

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, (), Error>;

struct AuxMetadataKey;

impl songbird::typemap::TypeMapKey for AuxMetadataKey {
    type Value = AuxMetadata;
}

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
    let metadata = src.aux_metadata().await.unwrap_or_else(|err| {
        warn!("Failed to get metadata for {url}: {err}");
        AuxMetadata {
            source_url: Some(url.clone()),
            ..Default::default()
        }
    });

    let mut vc = vc.lock().await;
    let track_handle = vc.enqueue(src.into()).await;

    // Attach description to the track handle so we can display each entry in the queue
    track_handle
        .typemap()
        .write()
        .await
        .insert::<AuxMetadataKey>(metadata);

    let queue_info = format_queue(vc.queue()).await;
    ctx.send(
        CreateReply::default()
            .content(format!("Added {url} to the queue"))
            .embed(queue_info),
    )
    .await?;

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
    let _ = vc.queue().skip();
    ctx.reply("Skipped the current song").await?;
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

fn format_metadata(meta: &AuxMetadata) -> String {
    let mut str = String::new();

    // Use the title if available, otherwise use the source URL
    if let Some(title) = &meta.title {
        str.push_str(&title);
    } else if let Some(source) = &meta.source_url {
        str.push_str(&source);
    } else {
        str.push_str("Unknown");
    }
    str = str.replace("rt_podcast", "Радио-Т ");

    // Append duration in mm:ss format if available
    if let Some(duration) = &meta.duration {
        let duration_secs = duration.as_secs();
        let mins = duration_secs / 60;
        let secs = duration_secs % 60;
        str.push_str(&format!(" {mins}:{secs:02}"));
    }

    str
}

async fn format_queue(queue: &TrackQueue) -> CreateEmbed {
    let mut queue_str = String::new();
    for track in queue.current_queue() {
        let typemap = track.typemap().read().await;
        let description = typemap.get::<AuxMetadataKey>().unwrap();

        use std::fmt::Write;
        let _ = write!(queue_str, "- {}\n", &format_metadata(description));
    }

    CreateEmbed::default().field("Queue:", queue_str, false)
}
