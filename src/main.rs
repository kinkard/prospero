use std::{collections::HashMap, env};

use serenity::{client::Client, prelude::GatewayIntents};
use songbird::SerenityInit;
use tokio::sync::RwLock;
use tracing::{info, warn};

mod commands;
mod events;
mod track_info;
mod yt_dlp;

#[derive(Default)]
struct Data {
    http_client: reqwest::Client,
    yt_dlp_cache: RwLock<HashMap<String, yt_dlp::YtDlp>>,
}
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Load env varialbes from .env is any
    if let Err(err) = dotenv::dotenv() {
        info!("Skipping .env file because of {err}");
    }

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected discord token in the environment");

    let framework = poise::Framework::builder()
        .setup(|ctx, _ready, framework: &poise::Framework<Data, Error>| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data::default())
            })
        })
        .options(poise::FrameworkOptions {
            commands: vec![
                commands::join(),
                commands::leave(),
                commands::ping(),
                commands::play(),
                commands::skip(),
                commands::stop(),
            ],
            ..Default::default()
        })
        .build();

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .event_handler(events::Handler)
        .framework(framework)
        .register_songbird()
        .await
        .expect("Failed to create discord client");

    let _ = client
        .start()
        .await
        .map_err(|why| warn!("Client stopped: {:?}", why));
}
