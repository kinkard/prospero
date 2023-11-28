use std::{env, sync::Arc};

use serenity::{client::Client, framework::StandardFramework, prelude::GatewayIntents};

use songbird::{driver::DecodeMode, Config, SerenityInit};

mod commands;
mod events;
mod player;
mod voice;

#[tokio::main]
async fn main() {
    // Load env varialbes from .env is any
    if let Err(err) = dotenv::dotenv() {
        println!("Skipping .env file because of {err}");
    }

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected discord token in the environment");

    let framework = StandardFramework::new()
        .configure(|c| c.prefix("/"))
        .group(&commands::GENERAL_GROUP);

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    // Skip all incoming voice packets by default to reduce CPU load.
    // We will change it on per-call basis if we want to listen listen other people.
    let songbird_config = Config::default().decode_mode(DecodeMode::Pass);

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
        .register_songbird_from_config(songbird_config)
        .await
        .expect("Failed to create discord client");

    let _ = client
        .start()
        .await
        .map_err(|why| println!("Client ended: {:?}", why));
}
