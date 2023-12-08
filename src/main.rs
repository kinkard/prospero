use std::{env, sync::Arc};

use serenity::{
    client::Client,
    framework::{standard::Configuration, StandardFramework},
    prelude::GatewayIntents,
};
use songbird::SerenityInit;

mod commands;
mod events;
mod player;

#[tokio::main]
async fn main() {
    // Load env varialbes from .env is any
    if let Err(err) = dotenv::dotenv() {
        println!("Skipping .env file because of {err}");
    }

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected discord token in the environment");

    let framework = StandardFramework::new().group(&commands::GENERAL_GROUP);
    framework.configure(Configuration::new().prefix("/"));

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let player = Arc::new(
        player::SpotifyPlayer::new(
            env::var("SPOTIFY_USERNAME").expect("Expected spotify username in the environment"),
            env::var("SPOTIFY_PASSWD").ok(),
            env::var("CACHE_LOCATION").ok(),
        )
        .await
        .expect("Failed to create spotify player"),
    );

    let mut client = Client::builder(&token, intents)
        .event_handler(events::Handler)
        .framework(framework)
        .type_map_insert::<player::SpotifyPlayerKey>(player)
        .register_songbird()
        .await
        .expect("Failed to create discord client");

    let _ = client
        .start()
        .await
        .map_err(|why| println!("Client ended: {:?}", why));
}
