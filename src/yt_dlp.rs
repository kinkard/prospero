use std::{
    collections::HashMap,
    io::ErrorKind,
    path::PathBuf,
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::Context;
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use reqwest::{
    header::{HeaderName, HeaderValue},
    Client,
};
use serde::Deserialize;
use songbird::input::{AudioStream, AudioStreamError, AuxMetadata, Compose, HttpRequest, Input};
use symphonia::core::io::MediaSource;
use tokio::{process::Command, sync::RwLock};
use tracing::{info, warn};

const YOUTUBE_DL_COMMAND: &str = "yt-dlp";

/// A thin wrapper around yt-dlp, providing a lazy request to select an audio stream
#[derive(Clone)]
pub(crate) struct YtDlp {
    http_request: HttpRequest,
    metadata: AuxMetadata,
}

impl YtDlp {
    pub(crate) async fn new(client: Client, url: &str) -> Result<Self, AudioStreamError> {
        let yt_dlp_output = Self::query(url).await?;

        let headers = yt_dlp_output
            .http_headers
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(k, v)| {
                        Some((
                            HeaderName::from_str(k).ok()?,
                            HeaderValue::from_str(v).ok()?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            http_request: HttpRequest {
                client,
                request: yt_dlp_output.url,
                headers,
                content_length: yt_dlp_output.filesize,
            },

            metadata: AuxMetadata {
                title: yt_dlp_output.title,
                artist: yt_dlp_output.artist.or(yt_dlp_output.uploader),
                album: yt_dlp_output.album,
                date: yt_dlp_output.release_date.or(yt_dlp_output.upload_date),
                channel: yt_dlp_output.channel,
                duration: yt_dlp_output.duration.map(Duration::from_secs_f64),
                source_url: yt_dlp_output.webpage_url,
                thumbnail: yt_dlp_output.thumbnail,
                ..AuxMetadata::default()
            },
        })
    }

    async fn query(query: &str) -> Result<YtDlpOutput, AudioStreamError> {
        let ytdl_args = [
            query,
            "-j",
            "-f",
            "ba[abr>0][vcodec=none]/best",
            "--no-playlist",
            "--default-search",
            "ytsearch",
        ];

        let command = Command::new(YOUTUBE_DL_COMMAND)
            .args(ytdl_args)
            .output()
            .await
            .map_err(|e| {
                AudioStreamError::Fail(if e.kind() == ErrorKind::NotFound {
                    format!("could not find executable '{}' on path", YOUTUBE_DL_COMMAND).into()
                } else {
                    Box::new(e)
                })
            })?;

        let yt_dlp_output: YtDlpOutput = serde_json::from_slice(&command.stdout)
            .map_err(|e| AudioStreamError::Fail(Box::new(e)))?;

        Ok(yt_dlp_output)
    }
}

impl From<YtDlp> for Input {
    fn from(val: YtDlp) -> Self {
        Input::Lazy(Box::new(val))
    }
}

#[async_trait]
impl Compose for YtDlp {
    fn create(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        Err(AudioStreamError::Unsupported)
    }

    async fn create_async(
        &mut self,
    ) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        self.http_request.create_async().await
    }

    fn should_create_async(&self) -> bool {
        true
    }

    async fn aux_metadata(&mut self) -> Result<AuxMetadata, AudioStreamError> {
        Ok(self.metadata.clone())
    }
}

#[derive(Deserialize)]
pub(crate) struct YtDlpOutput {
    artist: Option<String>,
    album: Option<String>,
    channel: Option<String>,
    duration: Option<f64>,
    filesize: Option<u64>,
    http_headers: Option<HashMap<String, String>>,
    release_date: Option<String>,
    thumbnail: Option<String>,
    title: Option<String>,
    upload_date: Option<String>,
    uploader: Option<String>,
    url: String,
    webpage_url: Option<String>,
}

#[derive(Default)]
pub(crate) struct Resolver {
    cache: RwLock<HashMap<String, YtDlp>>,
    cache_location: Option<PathBuf>,
    /// Unix timestamp of the last time the cache was updated
    last_update: AtomicU64,

    http_client: reqwest::Client,
}

impl Resolver {
    const CONCURRENCY: usize = 8;
    const CACHE_UPDATE_INTERVAL_SEC: u64 = 24 * 60 * 60;

    /// Creates a new yt-dlp resolver with a cache file
    pub(crate) fn new(cache_location: PathBuf) -> Self {
        Self {
            cache_location: Some(cache_location),
            ..Default::default()
        }
    }

    /// Loads cache from file and fetches all yt-dlp instances
    pub(crate) async fn load_cache(&self) {
        let Some(cache_location) = &self.cache_location else {
            // no-op if cache location is not set
            return;
        };

        let keys: Vec<String> = tokio::fs::read_to_string(cache_location)
            .await
            .context("Failed to read yt-dlp cache file")
            .and_then(|readed| {
                serde_json::from_str(&readed).context("Failed to parse yt-dlp cache json")
            })
            .unwrap_or_else(|err| {
                warn!("{err:#}");
                Vec::new()
            });
        self.update_inner(keys).await;
    }

    /// Saves cache to file
    pub(crate) async fn save_cache(&self) {
        if let Some(cache_location) = &self.cache_location {
            let serialized = {
                let cache = self.cache.read().await;
                let keys = cache.keys().collect::<Vec<_>>();
                serde_json::to_string(&keys).unwrap()
            };
            tokio::fs::write(cache_location, serialized).await.unwrap();
        }
    }

    /// Updates all yt-dlp instances in the cache
    pub(crate) async fn update_cache(&self) {
        // Request all YtDlp instances anew using the current cache keys
        let keys = {
            let cache = self.cache.read().await;
            cache.keys().cloned().collect::<Vec<_>>()
        };
        self.update_inner(keys).await;
    }

    /// Resolves a query to a yt-dlp instance, caching the result
    pub(crate) async fn resolve(&self, query: &str) -> Option<YtDlp> {
        // Two separate locks to avoid blocking everything on the long (up to 2s) yt-dlp query
        let cached_yt_dlp = self.cache.read().await.get(query).cloned();
        match cached_yt_dlp {
            Some(yt_dlp) => Some(yt_dlp),
            None => {
                let Some(yt_dlp) = Self::fetch(self.http_client.clone(), query).await else {
                    return None;
                };

                self.cache
                    .write()
                    .await
                    .insert(query.to_string(), yt_dlp.clone());
                Some(yt_dlp)
            }
        }
    }

    /// Inner function to fetch a yt-dlp instance
    async fn fetch(http_client: reqwest::Client, query: &str) -> Option<YtDlp> {
        let begin: std::time::Instant = std::time::Instant::now();
        let yt_dlp = match YtDlp::new(http_client, query).await {
            Ok(yt_dlp) => yt_dlp,
            Err(err) => {
                warn!("Failed to fetch '{query}' from yt-dlp: {err}");
                return None;
            }
        };
        info!(
            "Fetched {query} from yt-dlp in {}ms",
            begin.elapsed().as_millis()
        );
        Some(yt_dlp)
    }

    /// Updates the specified keys in the cache
    async fn update_inner(&self, keys: Vec<String>) {
        let last_update = self.last_update.load(Ordering::Relaxed);
        let unix_now = std::time::UNIX_EPOCH
            .elapsed()
            .map(|t| t.as_secs())
            // if by any chance this failed we will just update the cache
            .unwrap_or(last_update + Self::CACHE_UPDATE_INTERVAL_SEC);
        if unix_now - last_update < Self::CACHE_UPDATE_INTERVAL_SEC {
            // no-op if the cache is up to date
            return;
        }
        self.last_update.store(unix_now, Ordering::Relaxed);

        let items = stream::iter(keys)
            .map(|query| async move {
                let yt_dlp = Self::fetch(self.http_client.clone(), &query).await;
                (query, yt_dlp)
            })
            .buffer_unordered(Self::CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        // override existing cache with the new values and drop entry if it failed to fetch
        // so the following request will try it again instead of using the outdated value
        let mut cache = self.cache.write().await;
        for (query, yt_dlp) in items {
            if let Some(yt_dlp) = yt_dlp {
                cache.insert(query, yt_dlp);
            } else {
                cache.remove(&query);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn test_ytdlp_rick_roll() {
        let mut yt_dlp = YtDlp::new(Client::new(), "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .await
            .unwrap();

        let metadata = yt_dlp.aux_metadata().await.unwrap();

        assert_eq!(
            metadata.title,
            Some("Rick Astley - Never Gonna Give You Up (Official Music Video)".to_string())
        );
        assert_eq!(metadata.artist, Some("Rick Astley".to_string()));
        assert_eq!(metadata.duration, Some(Duration::from_secs(212)));
        assert_eq!(
            metadata.source_url,
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string())
        );
        assert_eq!(
            metadata.thumbnail,
            Some("https://i.ytimg.com/vi_webp/dQw4w9WgXcQ/maxresdefault.webp".to_string())
        );
    }

    #[tokio::test]
    async fn test_ytdlp_pritoptat() {
        let mut yt_dlp = YtDlp::new(Client::new(), "притоптать").await.unwrap();

        let metadata = yt_dlp.aux_metadata().await.unwrap();

        assert_eq!(
            metadata.title,
            Some("Нейромонах Феофан — Притоптать | Neuromonakh Feofan".to_string())
        );
        assert_eq!(metadata.artist, Some("Neuromonakh Feofan".to_string()));
        assert_eq!(metadata.duration, Some(Duration::from_secs(210)));
        assert_eq!(
            metadata.source_url,
            Some("https://www.youtube.com/watch?v=HNpLuXOg7xQ".to_string())
        );
        assert_eq!(
            metadata.thumbnail,
            Some("https://i.ytimg.com/vi/HNpLuXOg7xQ/maxresdefault.jpg".to_string())
        );
    }

    #[tokio::test]
    async fn test_ytdlp_radiot() {
        let mut yt_dlp = YtDlp::new(Client::new(), "https://cdn.radio-t.com/rt_podcast895.mp3")
            .await
            .unwrap();

        let metadata = yt_dlp.aux_metadata().await.unwrap();

        assert_eq!(metadata.title, Some("rt_podcast895".to_string()));
        assert_eq!(metadata.artist, None);
        assert_eq!(metadata.duration, None);
        assert_eq!(metadata.thumbnail, None);
        // cdn might resolve into different urls
        assert!(metadata
            .source_url
            .as_ref()
            .unwrap()
            .starts_with("https://"));
        println!("{:?}", metadata.source_url);
        assert!(metadata
            .source_url
            .unwrap()
            .ends_with(".radio-t.com/rtfiles/rt_podcast895.mp3"));
    }
}
