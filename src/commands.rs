use songbird::input::YoutubeDl;

use crate::http_client;

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, (), Error>;

/// Join my current voice channel
#[poise::command(guild_only, slash_command)]
pub(crate) async fn join(ctx: Context<'_>) -> Result<(), Error> {
    let (guild_id, channel_id) = {
        let guild = ctx.guild().unwrap();
        let channel_id = guild
            .voice_states
            .get(&ctx.author().id)
            .and_then(|voice_state| voice_state.channel_id);
        (guild.id, channel_id)
    };

    let Some(channel_id) = channel_id else {
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

#[poise::command(guild_only, slash_command)]
pub(crate) async fn play(ctx: Context<'_>, url: String) -> Result<(), Error> {
    let guild_id = ctx.guild().unwrap().id;

    if !url.starts_with("http") {
        ctx.reply("Must provide a valid URL that starts with `http`")
            .await?;
        return Ok(());
    }

    let http_client = http_client::get(ctx.serenity_context())
        .await
        .expect("HttpClient should be inserted in at initialisation");

    if let Some(vc) = songbird::get(ctx.serenity_context())
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .get(guild_id)
    {
        let mut vc = vc.lock().await;

        ctx.reply(format!("Playing {url}")).await?;

        let src = YoutubeDl::new(http_client, url);
        let _ = vc.play_only_input(src.into());
    } else {
        // todo: join vc message author belongs to
        ctx.reply("Not in a voice channel to play in").await?;
    }

    Ok(())
}
