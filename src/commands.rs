use serenity::{
    client::Context,
    framework::standard::{
        macros::{command, group},
        CommandResult,
    },
    model::channel::Message,
};

use crate::{player, voice};

#[group]
#[commands(join, leave, ping)]
struct General;

#[command]
#[only_in(guilds)]
async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let Some(channel_id) = guild
        .voice_states
        .get(&msg.author.id)
        .and_then(|state| state.channel_id)
    else {
        msg.reply(ctx, "You should be in a voice channel to invite me")
            .await?;
        return Ok(());
    };
    drop(guild); // we don't need it anymore

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let (vc_handler, conn_result) = manager.join(guild_id, channel_id).await;
    if let Ok(_) = conn_result {
        // NOTE: this skips listening for the actual connection result.
        let mut vc = vc_handler.lock().await;
        voice::Receiver::subscribe(&mut vc);

        let player = player::get(&ctx)
            .await
            .expect("Spotify Player should be placed in at initialisation");

        // 96k is a default Discord bitrate in guilds without nitro and we pull Spotify with 96k
        vc.set_bitrate(songbird::driver::Bitrate::BitsPerSecond(96_000));
        vc.play_only_source(player.audio_source());
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let player = player::get(&ctx)
        .await
        .expect("Spotify Player should be placed in at initialisation");

    player.stop();

    songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .remove(guild_id)
        .await?;

    Ok(())
}

#[command]
async fn ping(ctx: &Context, msg: &Message) -> CommandResult {
    msg.channel_id.say(&ctx.http, "Pong!").await?;
    Ok(())
}
