use std::{env, path::PathBuf, sync::Arc};

use serenity::{client::Client, prelude::GatewayIntents};
use songbird::SerenityInit;
use tokio::sync::Mutex;

mod commands;
mod events;
mod spotify;

#[tokio::main]
async fn main() {
    // Load env varialbes from .env is any
    if let Err(err) = dotenv::dotenv() {
        println!("Skipping .env file because of {err}");
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
                commands::connect_spotify(),
            ],
            ..Default::default()
        })
        .build();

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let data_dir = env::var("DATA_DIR").expect("Expected path to DATA in the environment");
    let mut db_path = PathBuf::from(data_dir);
    db_path.push("db.sqlite");

    let manager = Arc::new(Mutex::new(
        spotify::Manager::new(&db_path).expect("Failed to create spotify manager"),
    ));

    let mut client = Client::builder(&token, intents)
        .event_handler(events::Handler)
        .framework(framework)
        .type_map_insert::<spotify::ManagerKey>(manager)
        .register_songbird()
        .await
        .expect("Failed to create discord client");

    let _ = client
        .start()
        .await
        .map_err(|why| println!("Client ended: {:?}", why));
}
