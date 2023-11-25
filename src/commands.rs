use librespot::{core::mercury::MercuryError, playback::player::PlayerEvent};
use serenity::{
    client::Context,
    framework::standard::{
        macros::{command, group},
        CommandResult,
    },
    model::{channel::Message, gateway, user},
};
use songbird::input;

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

    let (handler_lock, conn_result) = manager.join(guild_id, channel_id).await;
    if let Ok(_) = conn_result {
        // NOTE: this skips listening for the actual connection result.
        let mut handler = handler_lock.lock().await;
        voice::Receiver::subscribe(&mut handler);

        let player = player::get(&ctx)
            .await
            .expect("Spotify Player should be placed in at initialisation");

        player.lock().await.enable_connect().await;

        // Handle Spotify events
        let c = ctx.clone();
        tokio::spawn(async move {
            loop {
                let channel = player.lock().await.event_channel.clone().unwrap();
                let mut receiver = channel.lock().await;

                let event = match receiver.recv().await {
                    Some(e) => e,
                    None => {
                        // Busy waiting bad but quick and easy
                        tokio::time::sleep(std::time::Duration::from_millis(256)).await;
                        continue;
                    }
                };

                match event {
                    PlayerEvent::Stopped { .. } => {
                        c.set_presence(None, user::OnlineStatus::Online).await;

                        let manager = songbird::get(&c)
                            .await
                            .expect("Songbird Voice client placed in at initialization.")
                            .clone();

                        let _ = manager.remove(guild_id).await;
                    }

                    PlayerEvent::Started { .. } => {
                        let manager = songbird::get(&c)
                            .await
                            .expect("Songbird Voice client placed in at initialization.")
                            .clone();

                        // we should be joined to the voice channel at that moment
                        if let Some(handler_lock) = manager.get(guild_id) {
                            let mut handler = handler_lock.lock().await;

                            let mut decoder = input::codec::OpusDecoderState::new().unwrap();
                            decoder.allow_passthrough = false;

                            let source = input::Input::new(
                                true,
                                input::reader::Reader::Extension(Box::new(
                                    player.lock().await.emitted_sink.clone(),
                                )),
                                input::codec::Codec::FloatPcm,
                                input::Container::Raw,
                                None,
                            );

                            handler.set_bitrate(songbird::driver::Bitrate::Auto);

                            handler.play_only_source(source);
                        } else {
                            println!("Could not fetch guild by ID.");
                        }
                    }

                    PlayerEvent::Paused { .. } => {
                        c.set_presence(None, user::OnlineStatus::Online).await;
                    }

                    PlayerEvent::Playing { track_id, .. } => {
                        let track: Result<librespot::metadata::Track, MercuryError> =
                            librespot::metadata::Metadata::get(
                                &player.lock().await.session,
                                track_id,
                            )
                            .await;

                        if let Ok(track) = track {
                            let artist: Result<librespot::metadata::Artist, MercuryError> =
                                librespot::metadata::Metadata::get(
                                    &player.lock().await.session,
                                    *track.artists.first().unwrap(),
                                )
                                .await;

                            if let Ok(artist) = artist {
                                let listening_to = format!("{}: {}", artist.name, track.name);

                                c.set_presence(
                                    Some(gateway::Activity::listening(listening_to)),
                                    user::OnlineStatus::Online,
                                )
                                .await;
                            }
                        }
                    }

                    _ => {}
                }
            }
        });
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    player::get(&ctx)
        .await
        .expect("Spotify Player should be placed in at initialisation")
        .lock()
        .await
        .disable_connect()
        .await;

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
