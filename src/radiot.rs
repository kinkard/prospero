use async_trait::async_trait;
use serde::Deserialize;
use songbird::input::{AudioStream, AudioStreamError, AuxMetadata, Compose, HttpRequest, Input};
use symphonia::core::io::MediaSource;
use tracing::warn;

use crate::track_info;

/// Resolver for Radio-T podcasts and live streams.
pub(crate) struct Resolver {
    http_client: reqwest::Client,
}

impl Resolver {
    pub(crate) fn new(http_client: reqwest::Client) -> Self {
        Self { http_client }
    }

    /// Resolves a query to a podcast.
    ///
    /// Possible inputs for live stream:
    /// - `https://stream.radio-t.com/`
    /// - `rt`
    /// - `рт`
    /// - `радио-т`
    ///
    /// Possible inputs for podcast:
    /// - `https://cdn.radio-t.com/rt_podcast{num}.mp3`
    /// - `rt{num}`
    /// - `rt {num}`
    /// - `рт{num}`
    /// - `рт {num}`
    /// - `радио-т {num}`
    pub(crate) async fn resolve(&self, query: &str) -> Option<Podcast> {
        let podcast = if let "https://stream.radio-t.com/" | "rt" | "рт" | "radio-t" | "радио-т" =
            query
        {
            // Return the last podcast if live stream is not online
            if self.stream_is_online().await {
                SiteApiResponse {
                    title: "Radio-T Online".to_string(),
                    url: "https://radio-t.com/".to_string(),
                    image: "https://radio-t.com/build/images/logo-icon.svg".to_string(),
                    audio_url: "https://stream.radio-t.com/".to_string(),
                }
            } else {
                self.http_client
                    .get("https://radio-t.com/site-api/last/1?categories=podcast")
                    .send()
                    .await
                    .ok()?
                    .json::<Vec<SiteApiResponse>>()
                    .await
                    .inspect_err(|err| {
                        warn!("Failed to parse Radio-T `/site-api/last` response: {err}");
                    })
                    .ok()?
                    .pop()?
            }
        } else if let Some(num) = query
            .strip_prefix("https://cdn.radio-t.com/rt_podcast")
            .and_then(|rem| rem.strip_suffix(".mp3"))
            .or_else(|| query.strip_prefix("rt"))
            .or_else(|| query.strip_prefix("рт"))
            .or_else(|| query.strip_prefix("radio-t"))
            .or_else(|| query.strip_prefix("радио-т"))
            .and_then(|num| num.trim().parse::<u16>().ok())
        {
            self.http_client
                .get(format!("https://radio-t.com/site-api/podcast/{num}"))
                .send()
                .await
                .ok()?
                .json::<SiteApiResponse>()
                .await
                .inspect_err(|err| {
                    warn!("Failed to parse Radio-T `/site-api/podcast` response: {err}");
                })
                .ok()?
        } else {
            return None;
        };

        Some(Podcast {
            http_request: HttpRequest::new(self.http_client.clone(), podcast.audio_url),
            metadata: track_info::Metadata {
                title: podcast.title.into_boxed_str(),
                source_url: podcast.url.into_boxed_str(),
                thumbnail_url: Some(podcast.image.into_boxed_str()),
                duration_sec: None,
            },
        })
    }

    async fn stream_is_online(&self) -> bool {
        self.http_client
            .head("https://stream.radio-t.com/")
            .send()
            .await
            .is_ok_and(|response| {
                response
                    .headers()
                    .get("content-type")
                    .is_some_and(|content_type| {
                        content_type == "audio/mpeg" || content_type == "audio/ogg"
                    })
            })
    }
}

pub(crate) struct Podcast {
    http_request: HttpRequest,
    metadata: track_info::Metadata,
}

impl Podcast {
    pub(crate) const fn metadata(&self) -> &track_info::Metadata {
        &self.metadata
    }
}

impl From<Podcast> for Input {
    fn from(val: Podcast) -> Self {
        Input::Lazy(Box::new(val))
    }
}

#[async_trait]
impl Compose for Podcast {
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

/// Response from the Radio-T Site API.
/// See more details in https://radio-t.com/api-docs/
#[derive(Deserialize)]
struct SiteApiResponse {
    /// Podcast title
    title: String,
    /// Web page URL
    url: String,
    /// Podcast thumbnail URL
    image: String,
    /// Podcast audio URL
    audio_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn resolve_live_stream_test() {
        let resolver = Resolver {
            http_client: reqwest::Client::new(),
        };

        let queries = [
            "https://stream.radio-t.com/",
            "rt",
            "рт",
            "radio-t",
            "радио-т",
        ];

        for query in queries {
            assert!(resolver.resolve(query).await.is_some());
        }
    }

    #[tokio::test]
    async fn resolve_podcast_test() {
        let resolver = Resolver {
            http_client: reqwest::Client::new(),
        };

        let queries = [
            "https://cdn.radio-t.com/rt_podcast912.mp3",
            "rt912",
            "rt 912",
            "рт912",
            "рт 912",
            "radio-t 912",
            "радио-т 912",
        ];

        for query in queries {
            let podcast = resolver.resolve(query).await;
            assert!(podcast.is_some(), "Failed to resolve query: {}", query);
            let podcast = podcast.unwrap();

            assert_eq!(podcast.metadata.title, "Радио-Т 912".into());
            assert!(podcast.metadata.source_url.ends_with("/podcast-912/"));
            assert_eq!(
                podcast.metadata.thumbnail_url,
                Some("https://radio-t.com/images/radio-t/rt912.jpg".into())
            );
            assert_eq!(
                podcast.http_request.request,
                "http://cdn.radio-t.com/rt_podcast912.mp3"
            );
        }
    }

    #[tokio::test]
    async fn resolve_fail_test() {
        let resolver = Resolver {
            http_client: reqwest::Client::new(),
        };

        let queries = [
            "my random query",
            "https://www.youtube.com/watch?v=wyaWZYM9Oa8",
            "invalid",
            "rt999999",
            "рт999999",
            "radio-t 999999",
            "радио-т 999999",
        ];

        for query in queries {
            assert!(resolver.resolve(query).await.is_none());
        }
    }
}
