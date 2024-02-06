use std::env;

use serenity::{client::Client, prelude::GatewayIntents};
use songbird::SerenityInit;
use tracing::{info, warn};

mod commands;
mod events;
mod http_client;

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
        .setup(
            |ctx, _ready, framework: &poise::Framework<(), commands::Error>| {
                Box::pin(async move {
                    poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                    Ok(())
                })
            },
        )
        .options(poise::FrameworkOptions {
            commands: vec![
                commands::join(),
                commands::leave(),
                commands::ping(),
                commands::play(),
            ],
            ..Default::default()
        })
        .build();

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .event_handler(events::Handler)
        .framework(framework)
        .type_map_insert::<http_client::HttpClientKey>(http_client::HttpClient::new())
        .register_songbird()
        .await
        .expect("Failed to create discord client");

    let _ = client
        .start()
        .await
        .map_err(|why| warn!("Client stopped: {:?}", why));
}
