use std::borrow::Cow;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::str::FromStr;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

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

use crate::track_info;

const YOUTUBE_DL_COMMAND: &str = "yt-dlp";

/// A thin wrapper around yt-dlp, providing a lazy request to select an audio stream
#[derive(Clone)]
pub(crate) struct YtDlp {
    http_request: HttpRequest,
    metadata: track_info::Metadata,
}

impl YtDlp {
    pub(crate) async fn new(client: Client, query: &str) -> Result<Self, AudioStreamError> {
        let yt_dlp_output = Self::query(query).await?;

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

        let title = yt_dlp_output
            .title
            .unwrap_or_else(|| query.to_string())
            .into_boxed_str();

        let source_url = if query.starts_with("http") {
            query.to_string()
        } else if let Some(url) = yt_dlp_output.webpage_url {
            url
        } else {
            format!("https://www.youtube.com/results?search_query={query}")
        }
        .into_boxed_str();

        Ok(Self {
            http_request: HttpRequest {
                client,
                request: yt_dlp_output.url,
                headers,
                content_length: yt_dlp_output.filesize,
            },
            metadata: track_info::Metadata {
                title,
                source_url,
                thumbnail_url: yt_dlp_output.thumbnail.map(String::into_boxed_str),
                duration_sec: yt_dlp_output
                    .duration
                    .map(|d| d as u32)
                    .and_then(std::num::NonZeroU32::new),
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
                    format!("could not find executable '{YOUTUBE_DL_COMMAND}' on path").into()
                } else {
                    Box::new(e)
                })
            })?;

        let yt_dlp_output: YtDlpOutput = serde_json::from_slice(&command.stdout)
            .map_err(|e| AudioStreamError::Fail(Box::new(e)))?;

        Ok(yt_dlp_output)
    }

    /// Provides track metadata
    pub(crate) const fn metadata(&self) -> &track_info::Metadata {
        &self.metadata
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
        Ok(self.metadata.clone().into())
    }
}

#[derive(Deserialize)]
pub(crate) struct YtDlpOutput {
    // artist: Option<String>,
    // album: Option<String>,
    // channel: Option<String>,
    duration: Option<f64>,
    filesize: Option<u64>,
    http_headers: Option<HashMap<String, String>>,
    // release_date: Option<String>,
    thumbnail: Option<String>,
    title: Option<String>,
    // upload_date: Option<String>,
    // uploader: Option<String>,
    url: String,
    webpage_url: Option<String>,
}

/// Query cache for yt-dlp that helps to reduce time spent on searching YouTube
pub(crate) trait QueryCache: Send + Sync {
    /// Saves found webpage_url for the query
    fn save(&self, query: &str, webpage_url: &str) -> Result<(), anyhow::Error>;
    /// Loads found webpage_url for the query if it is known
    fn load(&self, query: &str) -> Option<String>;
    /// Returns all known webpage_urls
    fn load_all(&self) -> Vec<String>;
}

pub(crate) struct Resolver {
    /// Query to webpage_url cache to speed up the yt-dlp queries
    query_cache: Arc<dyn QueryCache>,
    /// Recently fetched yt-dlp instances
    cache: RwLock<HashMap<String, YtDlp>>,
    /// Unix timestamp of the last time the cache was updated
    last_update: AtomicU64,

    http_client: reqwest::Client,
}

impl Resolver {
    const CONCURRENCY: usize = 8;
    const CACHE_UPDATE_INTERVAL_SEC: u64 = 24 * 60 * 60;

    /// Creates a new yt-dlp resolver with a cache file
    pub(crate) fn new(http_client: reqwest::Client, cache: Arc<dyn QueryCache>) -> Self {
        Self {
            query_cache: cache,
            http_client,
            cache: RwLock::new(HashMap::new()),
            last_update: AtomicU64::new(0),
        }
    }

    /// Loads cache from file and fetches all yt-dlp instances
    pub(crate) async fn load_cache(&self) {
        self.update_inner(self.query_cache.load_all()).await;
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
        // For non-URL queries, check the cache first
        let query = if !query.starts_with("http") {
            if let Some(webpage_url) = self.query_cache.load(query) {
                Cow::from(webpage_url)
            } else {
                Cow::from(query)
            }
        } else {
            Cow::from(query)
        };

        // Two separate locks to avoid blocking everything on the long (up to 2s) yt-dlp query
        let cached_yt_dlp = self.cache.read().await.get(query.as_ref()).cloned();
        match cached_yt_dlp {
            Some(yt_dlp) => Some(yt_dlp),
            None => {
                let yt_dlp = Self::fetch(self.http_client.clone(), query.as_ref()).await?;

                // Save the query to webpage_url mapping if it was not a URL query
                if !query.starts_with("http") {
                    if let Err(err) = self
                        .query_cache
                        .save(query.as_ref(), &yt_dlp.metadata.source_url)
                    {
                        warn!("Failed to save yt-dlp query '{query}' to cache: {err}");
                    }
                }

                self.cache
                    .write()
                    .await
                    .insert(yt_dlp.metadata.source_url.clone().into(), yt_dlp.clone());
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
            info!("Cache is up to date, skipping update");
            return;
        }
        self.last_update.store(unix_now, Ordering::Relaxed);

        let items = stream::iter(keys)
            .map(|query| async move {
                let yt_dlp = Self::fetch(self.http_client.clone(), &query).await;
                (query, yt_dlp)
            })
            .buffer_unordered(Self::CONCURRENCY)
            .filter_map(|(query, yt_dlp)| async move { yt_dlp.map(|yt_dlp| (query, yt_dlp)) })
            .collect::<HashMap<_, _>>()
            .await;

        // override existing cache with the new values and drop entry if it failed to fetch
        // so the following request will try it again instead of using the outdated value
        *self.cache.write().await = items;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    #[ignore]
    #[tokio::test]
    async fn resolve_rick_roll() {
        let mut yt_dlp = YtDlp::new(Client::new(), "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
            .await
            .unwrap();

        let metadata = yt_dlp.aux_metadata().await.unwrap();

        assert_eq!(
            metadata.title,
            Some("Rick Astley - Never Gonna Give You Up (Official Music Video)".to_string())
        );
        // assert_eq!(metadata.artist, Some("Rick Astley".to_string()));
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

    #[ignore]
    #[tokio::test]
    async fn resolve_pritoptat() {
        let mut yt_dlp = YtDlp::new(Client::new(), "притоптать").await.unwrap();

        let metadata = yt_dlp.aux_metadata().await.unwrap();

        assert_eq!(
            metadata.title,
            Some(
                "Нейромонах Феофан - Притоптать (official video) | Neuromonakh Feofan".to_string()
            )
        );
        // assert_eq!(metadata.artist, Some("Neuromonakh Feofan".to_string()));
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
}
