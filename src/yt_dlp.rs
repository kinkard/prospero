use async_trait::async_trait;
use reqwest::{
    header::{HeaderName, HeaderValue},
    Client,
};
use serde::Deserialize;
use songbird::input::{AudioStream, AudioStreamError, AuxMetadata, Compose, HttpRequest, Input};
use std::{collections::HashMap, io::ErrorKind, str::FromStr, time::Duration};
use symphonia::core::io::MediaSource;
use tokio::process::Command;

const YOUTUBE_DL_COMMAND: &str = "yt-dlp";

/// A thin wrapper around yt-dlp, providing a lazy request to select an audio stream
pub struct YtDlp {
    http_request: HttpRequest,
    metadata: AuxMetadata,
}

impl YtDlp {
    pub async fn new(client: Client, url: &str) -> Result<Self, AudioStreamError> {
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
pub struct YtDlpOutput {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub channel: Option<String>,
    pub duration: Option<f64>,
    pub filesize: Option<u64>,
    pub http_headers: Option<HashMap<String, String>>,
    pub release_date: Option<String>,
    pub thumbnail: Option<String>,
    pub title: Option<String>,
    pub track: Option<String>,
    pub upload_date: Option<String>,
    pub uploader: Option<String>,
    pub url: String,
    pub webpage_url: Option<String>,
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
