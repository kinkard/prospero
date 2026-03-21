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

struct Data {
    yt_dlp_resolver: yt_dlp::Resolver,
}
type Context<'a> = poise::Context<'a, Data, anyhow::Error>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Load env varialbes from .env if any
    if let Err(err) = dotenv::dotenv() {
        info!("Skipping .env file because of {err}");
    }

    let http_client = reqwest::Client::new();
    let bot_data = Data {
        yt_dlp_resolver: yt_dlp::Resolver::new(http_client.clone()),
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
