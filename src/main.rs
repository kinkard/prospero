use std::env;

use serenity::{
    client::{Client, FullEvent},
    prelude::GatewayIntents,
};
use songbird::SerenityInit;
use tracing::{info, warn};

mod commands;
mod events;
mod radiot;
#[cfg(feature = "spotify")]
mod spotify;
mod storage;
mod track_info;
mod yt_dlp;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct Data {
    yt_dlp_resolver: yt_dlp::Resolver,
    radio_t_resolver: radiot::Resolver,
    #[cfg(feature = "spotify")]
    spotify_resolver: spotify::Resolver,
}
type Context<'a> = poise::Context<'a, Data, anyhow::Error>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Load env varialbes from .env if any
    if let Err(err) = dotenv::dotenv() {
        info!("Skipping .env file because of {err}");
    }

    let data_dir = env::var("DATA_DIR").expect("Expected path to DATA in the environment");
    let data_dir = std::path::PathBuf::from(data_dir);
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir).expect("Failed to create yt-dlp cache directory");
    }

    let storage =
        storage::Storage::new(data_dir.join("db.sqlite")).expect("Failed to create storage");

    let http_client = reqwest::Client::new();
    let bot_data = Data {
        #[cfg(feature = "spotify")]
        spotify_resolver: spotify::Resolver::new(storage.clone()),
        yt_dlp_resolver: yt_dlp::Resolver::new(http_client.clone(), storage),
        radio_t_resolver: radiot::Resolver::new(http_client.clone()),
    };

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected discord token in the environment");

    let framework = poise::Framework::builder()
        .setup(
            |ctx, _ready, framework: &poise::Framework<Data, anyhow::Error>| {
                Box::pin(async move {
                    poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                    Ok(bot_data)
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
                #[cfg(feature = "spotify")]
                commands::connect_spotify(),
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

    let mut client = Client::builder(&token, GatewayIntents::non_privileged())
        .framework(framework)
        .register_songbird()
        .await
        .expect("Failed to create discord client");

    let _ = client
        .start()
        .await
        .map_err(|why| warn!("Client stopped: {:?}", why));
}
