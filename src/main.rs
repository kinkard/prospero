use std::{env, path::PathBuf, sync::Arc};

use serenity::{
    client::{Client, FullEvent},
    prelude::GatewayIntents,
};
use songbird::SerenityInit;
use tokio::sync::Mutex;
use tracing::{info, warn};

mod commands;
mod events;
mod spotify;
mod track_info;
mod yt_dlp;

struct Data {
    yt_dlp_resolver: yt_dlp::Resolver,
    spotify_manager: Arc<Mutex<spotify::Manager>>,
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
    let data_dir = PathBuf::from(data_dir);
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir).expect("Failed to create yt-dlp cache directory");
    }

    let yt_dlp_resolver = {
        let mut data_dir = data_dir.clone();
        data_dir.push("yt-dlp-cache.json");
        yt_dlp::Resolver::new(data_dir)
    };

    let spotify_manager = {
        let mut db_path = data_dir;
        db_path.push("db.sqlite");
        Arc::new(Mutex::new(
            spotify::Manager::new(&db_path).expect("Failed to create spotify manager"),
        ))
    };

    let token = env::var("DISCORD_TOKEN").expect("Expected discord token in the environment");

    let framework = poise::Framework::builder()
        .setup(
            |ctx, _ready, framework: &poise::Framework<Data, anyhow::Error>| {
                Box::pin(async move {
                    poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                    Ok(Data {
                        yt_dlp_resolver,
                        spotify_manager,
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
