use serenity::{
    client::Context,
    framework::standard::{
        macros::{command, group},
        CommandResult,
    },
    model::channel::Message,
};

#[group]
#[commands(join, leave, ping)]
struct General;

#[command]
#[only_in(guilds)]
async fn join(ctx: &Context, msg: &Message) -> CommandResult {
    let (guild_id, channel_id) = {
        let guild = msg.guild(&ctx.cache).unwrap();
        let channel_id = guild
            .voice_states
            .get(&msg.author.id)
            .and_then(|voice_state| voice_state.channel_id);

        (guild.id, channel_id)
    };

    let Some(channel_id) = channel_id else {
        msg.reply(ctx, "You should be in a voice channel to invite me")
            .await?;
        return Ok(());
    };

    let _vc_handler = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .join(guild_id, channel_id)
        .await;

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = msg.guild(&ctx.cache).unwrap().id;

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
