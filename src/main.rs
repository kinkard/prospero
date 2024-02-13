use std::env;

use serenity::{
    client::{Client, FullEvent},
    prelude::GatewayIntents,
};
use songbird::SerenityInit;
use tracing::{info, warn};

mod commands;
mod events;
mod track_info;
mod yt_dlp;

#[derive(Default)]
struct Data {
    yt_dlp_resolver: yt_dlp::Resolver,
}
type Context<'a> = poise::Context<'a, Data, anyhow::Error>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Load env varialbes from .env is any
    if let Err(err) = dotenv::dotenv() {
        info!("Skipping .env file because of {err}");
    }

    let data_dir = env::var("DATA_DIR").expect("Expected path to DATA in the environment");
    let mut data_dir = std::path::PathBuf::from(data_dir);
    data_dir.push("yt-dlp-cache.json");

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected discord token in the environment");

    let framework = poise::Framework::builder()
        .setup(
            |ctx, _ready, framework: &poise::Framework<Data, anyhow::Error>| {
                Box::pin(async move {
                    poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                    Ok(Data {
                        yt_dlp_resolver: yt_dlp::Resolver::new(data_dir),
                    })
                })
            },
        )
        .options(poise::FrameworkOptions {
            commands: vec![
                commands::join(),
                commands::leave(),
                commands::ping(),
                commands::play(),
                commands::skip(),
                commands::stop(),
            ],
            event_handler: |ctx, event, _framework, data| {
                Box::pin(async move {
                    match event {
                        FullEvent::CacheReady { guilds } => {
                            events::cache_ready(ctx, data, guilds).await;
                        }
                        FullEvent::VoiceStateUpdate { old, new } => {
                            events::voice_state_update(ctx, data, old, new).await;
                        }
                        _ => (),
                    }
                    Ok(())
                })
            },
            ..Default::default()
        })
        .build();

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .framework(framework)
        .register_songbird()
        .await
        .expect("Failed to create discord client");

    let _ = client
        .start()
        .await
        .map_err(|why| warn!("Client stopped: {:?}", why));
}
