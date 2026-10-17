use std::borrow::Cow;
use std::collections::HashMap;
use std::str::FromStr;

use anyhow::{Context as _, anyhow};
use async_trait::async_trait;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use reqwest::{
    Client,
    header::{HeaderName, HeaderValue},
};
use songbird::input::{AudioStream, AudioStreamError, AuxMetadata, Compose, HttpRequest, Input};
use symphonia::core::io::MediaSource;
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::{info, warn};

use crate::track_info;

const QUEUE_DEPTH: usize = 32;

type Reply = oneshot::Sender<anyhow::Result<Output>>;

#[derive(Clone)]
struct YtDlp {
    tx: mpsc::Sender<(String, Reply)>,
}

impl YtDlp {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel(QUEUE_DEPTH);
        std::thread::spawn(move || resolver_thread(rx));
        Self { tx }
    }

    async fn resolve(&self, query: &str) -> anyhow::Result<Output> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send((query.to_owned(), reply))
            .await
            .map_err(|_| anyhow!("resolver is gone"))?;
        rx.await
            .map_err(|_| anyhow!("resolver dropped the request"))?
    }
}

fn resolver_thread(mut rx: mpsc::Receiver<(String, Reply)>) {
    Python::initialize();
    let ydl: Py<PyAny> = match Python::try_attach(create_ydl) {
        Some(Ok(ydl)) => ydl,
        err => {
            let msg = match err {
                Some(Err(e)) => format!("{e:#}"),
                _ => "Python not available".into(),
            };
            warn!("failed to initialize yt-dlp: {msg}");
            while let Some((_, reply)) = rx.blocking_recv() {
                let _ = reply.send(Err(anyhow!("yt-dlp init failed: {msg}")));
            }
            return;
        }
    };

    while let Some((query, reply)) = rx.blocking_recv() {
        let result = Python::try_attach(|py| extract(py, ydl.bind(py), &query))
            .unwrap_or_else(|| Err(anyhow!("Python not available")));
        let _ = reply.send(result);
    }
}

fn create_ydl(py: Python<'_>) -> anyhow::Result<Py<PyAny>> {
    let yt_dlp = py.import("yt_dlp").context("import yt_dlp")?;
    let opts = PyDict::new(py);
    for (k, v) in [
        ("quiet", true),
        ("no_warnings", true),
        ("noprogress", true),
        ("logtostderr", true),
        ("no_color", true),
        ("simulate", true),
        ("skip_download", true),
        ("noplaylist", true),
    ] {
        opts.set_item(k, v)?;
    }
    opts.set_item("format", "bestaudio")?;
    opts.set_item("default_search", "ytsearch")?;
    opts.set_item("playlist_items", "1")?;
    let ydl = yt_dlp.getattr("YoutubeDL")?.call1((&opts,))?;
    Ok(ydl.unbind())
}

fn extract(py: Python<'_>, ydl: &Bound<'_, PyAny>, query: &str) -> anyhow::Result<Output> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("download", false)?;
    let info = ydl
        .call_method("extract_info", (query,), Some(&kwargs))
        .map_err(|e| anyhow!("{e}"))?;

    if info.is_none() {
        anyhow::bail!("nothing extracted");
    }

    let info = if py_str(&info, "_type")?.as_deref() == Some("playlist") {
        let entries: Vec<Bound<'_, PyAny>> = info
            .call_method1("get", ("entries",))?
            .try_iter()?
            .filter_map(|e| e.ok())
            .filter(|e| !e.is_none())
            .collect();
        entries
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no results"))?
    } else {
        info
    };

    let url: String = py_str(&info, "url")?.ok_or_else(|| anyhow!("no playable format"))?;

    let http_headers: Option<HashMap<String, String>> =
        info.call_method1("get", ("http_headers",))?.extract().ok();

    Ok(Output {
        url,
        title: py_str(&info, "title")?,
        duration: py_f64(&info, "duration")?,
        thumbnail: py_str(&info, "thumbnail")?,
        webpage_url: py_str(&info, "webpage_url")?,
        filesize: py_u64(&info, "filesize")?,
        http_headers,
    })
}

fn py_str(obj: &Bound<'_, PyAny>, key: &str) -> PyResult<Option<String>> {
    let val = obj.call_method1("get", (key,))?;
    if val.is_none() {
        return Ok(None);
    }
    Ok(val.extract().ok())
}

fn py_f64(obj: &Bound<'_, PyAny>, key: &str) -> PyResult<Option<f64>> {
    let val = obj.call_method1("get", (key,))?;
    if val.is_none() {
        return Ok(None);
    }
    Ok(val.extract().ok())
}

fn py_u64(obj: &Bound<'_, PyAny>, key: &str) -> PyResult<Option<u64>> {
    let val = obj.call_method1("get", (key,))?;
    if val.is_none() {
        return Ok(None);
    }
    Ok(val.extract().ok())
}

#[derive(Clone)]
pub(crate) struct Track {
    http_request: HttpRequest,
    metadata: track_info::Metadata,
}

impl Track {
    fn from_output(client: Client, query: &str, output: Output) -> Self {
        let headers = output
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

        let title = output
            .title
            .unwrap_or_else(|| query.to_string())
            .into_boxed_str();

        let source_url = if query.starts_with("http") {
            query.to_string()
        } else if let Some(url) = output.webpage_url {
            url
        } else {
            format!("https://www.youtube.com/results?search_query={query}")
        }
        .into_boxed_str();

        Self {
            http_request: HttpRequest {
                client,
                request: output.url,
                headers,
                content_length: output.filesize,
            },
            metadata: track_info::Metadata {
                title,
                source_url,
                thumbnail_url: output.thumbnail.map(String::into_boxed_str),
                duration_sec: output
                    .duration
                    .map(|d| d as u32)
                    .and_then(std::num::NonZeroU32::new),
            },
        }
    }

    pub(crate) const fn metadata(&self) -> &track_info::Metadata {
        &self.metadata
    }
}

impl From<Track> for Input {
    fn from(val: Track) -> Self {
        Input::Lazy(Box::new(val))
    }
}

#[async_trait]
impl Compose for Track {
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

struct Output {
    duration: Option<f64>,
    filesize: Option<u64>,
    http_headers: Option<HashMap<String, String>>,
    thumbnail: Option<String>,
    title: Option<String>,
    url: String,
    webpage_url: Option<String>,
}

#[derive(Clone)]
struct CacheEntry {
    loaded_at: std::time::Instant,
    track: Track,
}

pub(crate) struct Resolver {
    query_to_url: RwLock<HashMap<String, String>>,
    cache: RwLock<HashMap<String, CacheEntry>>,
    http_client: reqwest::Client,
    backend: YtDlp,
}

impl Resolver {
    const CACHE_EXPIRATION: std::time::Duration = std::time::Duration::from_secs(60 * 60);

    pub(crate) fn new(http_client: reqwest::Client) -> Self {
        Self {
            query_to_url: RwLock::new(HashMap::new()),
            http_client,
            cache: RwLock::new(HashMap::new()),
            backend: YtDlp::new(),
        }
    }

    pub(crate) async fn resolve(&self, query: &str) -> Option<Track> {
        let query = if !query.starts_with("http") {
            if let Some(webpage_url) = self.query_to_url.read().await.get(query).cloned() {
                Cow::from(webpage_url)
            } else {
                Cow::from(query)
            }
        } else {
            Cow::from(query)
        };

        let cached = self.cache.read().await.get(query.as_ref()).cloned();
        if let Some(entry) = cached
            && entry.loaded_at.elapsed() < Self::CACHE_EXPIRATION
        {
            return Some(entry.track);
        }

        let begin = std::time::Instant::now();
        let output = match self.backend.resolve(&query).await {
            Ok(output) => output,
            Err(err) => {
                warn!("Failed to fetch '{query}' from yt-dlp: {err}");
                return None;
            }
        };
        let track = Track::from_output(self.http_client.clone(), &query, output);
        info!(
            "Fetched {query} from yt-dlp in {}ms",
            begin.elapsed().as_millis()
        );

        if !query.starts_with("http") {
            self.query_to_url
                .write()
                .await
                .insert(query.into_owned(), track.metadata.source_url.clone().into());
        }

        self.cache.write().await.insert(
            track.metadata.source_url.clone().into(),
            CacheEntry {
                loaded_at: std::time::Instant::now(),
                track: track.clone(),
            },
        );
        Some(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    async fn resolve_track(yt_dlp: &YtDlp, query: &str) -> Track {
        let output = yt_dlp.resolve(query).await.unwrap();
        Track::from_output(Client::new(), query, output)
    }

    #[ignore]
    #[tokio::test]
    async fn resolve_shape() {
        let yt_dlp = YtDlp::new();
        let track = resolve_track(&yt_dlp, "https://www.youtube.com/watch?v=dQw4w9WgXcQ").await;

        let metadata = track.metadata();
        assert!(!metadata.title.is_empty(), "title should be populated");
        assert!(
            metadata.duration_sec.is_some(),
            "duration should be present"
        );
        assert!(
            metadata.thumbnail_url.is_some(),
            "thumbnail should be present"
        );
        assert_eq!(
            &*metadata.source_url,
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );

        assert!(track.http_request.request.starts_with("https://"));
        assert!(
            track.http_request.content_length.is_some(),
            "filesize must survive, or playback is throttled ~90x"
        );
    }

    #[ignore]
    #[tokio::test]
    async fn survives_a_bad_query() {
        let yt_dlp = YtDlp::new();
        assert!(
            yt_dlp
                .resolve("https://www.youtube.com/watch?v=zzzzzzzzzzz")
                .await
                .is_err()
        );
        assert!(
            yt_dlp
                .resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
                .await
                .is_ok(),
            "resolver should still serve requests after a failure"
        );
    }

    #[ignore]
    #[tokio::test]
    async fn resolve_rick_roll() {
        let yt_dlp = YtDlp::new();
        let mut track = resolve_track(&yt_dlp, "https://www.youtube.com/watch?v=dQw4w9WgXcQ").await;

        let metadata = track.aux_metadata().await.unwrap();
        assert_eq!(
            metadata.title,
            Some("Rick Astley - Never Gonna Give You Up (Official Music Video)".to_string())
        );
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
        let yt_dlp = YtDlp::new();
        let mut track = resolve_track(
            &yt_dlp,
            "\u{043F}\u{0440}\u{0438}\u{0442}\u{043E}\u{043F}\u{0442}\u{0430}\u{0442}\u{044C}",
        )
        .await;

        let metadata = track.aux_metadata().await.unwrap();
        assert_eq!(
            metadata.title,
            Some(
                "\u{041D}\u{0435}\u{0439}\u{0440}\u{043E}\u{043C}\u{043E}\u{043D}\u{0430}\u{0445} \u{0424}\u{0435}\u{043E}\u{0444}\u{0430}\u{043D} - \u{041F}\u{0440}\u{0438}\u{0442}\u{043E}\u{043F}\u{0442}\u{0430}\u{0442}\u{044C} (official video) | Neuromonakh Feofan".to_string()
            )
        );
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
