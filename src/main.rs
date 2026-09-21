use std::collections::HashMap;
use std::env;
use std::sync::RwLock;

use serenity::{
    client::{Client, FullEvent},
    model::id::GuildId,
    prelude::GatewayIntents,
};
use songbird::SerenityInit;
use tracing::{info, warn};

mod commands;
mod events;
mod track_info;
mod yt_dlp;

const DEFAULT_VOLUME: u8 = 5;

struct Data {
    yt_dlp_resolver: yt_dlp::Resolver,
    /// Per-guild volume level (0-10). Absent means DEFAULT_VOLUME.
    guild_volumes: RwLock<HashMap<GuildId, u8>>,
}

/// Maps a 0..=10 user-facing volume to a 0.0..=1.0 linear amplitude.
/// Uses an exponential curve so that 0→0.0, 5→0.05, 10→1.0.
fn volume_to_amplitude(level: u8) -> f32 {
    if level == 0 {
        return 0.0;
    }
    (19.0_f32.powf(level as f32 / 5.0) - 1.0) / 360.0
}
type Context<'a> = poise::Context<'a, Data, anyhow::Error>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Load env variables from .env if any
    if let Err(err) = dotenv::dotenv() {
        info!("Skipping .env file because of {err}");
    }

    let http_client = reqwest::Client::new();
    let bot_data = Data {
        yt_dlp_resolver: yt_dlp::Resolver::new(http_client.clone()),
        guild_volumes: RwLock::new(HashMap::new()),
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
                commands::volume(),
            ],
            event_handler: |framework, event| {
                Box::pin(async move {
                    let (ctx, data) = (framework.serenity_context, framework.user_data);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_to_amplitude_key_points() {
        assert_eq!(volume_to_amplitude(0), 0.0);
        assert!((volume_to_amplitude(5) - 0.05).abs() < 1e-6);
        assert!((volume_to_amplitude(10) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn volume_to_amplitude_is_monotonic() {
        for level in 1..=10 {
            assert!(
                volume_to_amplitude(level) > volume_to_amplitude(level - 1),
                "volume_to_amplitude({level}) should be > volume_to_amplitude({})",
                level - 1
            );
        }
    }
}
