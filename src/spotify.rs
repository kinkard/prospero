use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use librespot::core::{
    config::SessionConfig, session::Session, spotify_id::SpotifyItemType, SpotifyId,
};
use librespot::discovery::Credentials;
use librespot::metadata::{self, image::ImageSize, Metadata};
use librespot::playback::{
    audio_backend::{self, SinkError, SinkResult},
    config::PlayerConfig,
    convert::Converter,
    decoder::AudioPacket,
    mixer::NoOpVolume,
    player,
};
use smallvec::{smallvec, SmallVec};
use songbird::input::{
    core::io::MediaSource, AudioStream, AudioStreamError, AuxMetadata, Compose, Input,
};
use tracing::info;

type ByteSink = flume::Sender<Box<[u8]>>;
type ByteStream = flume::Receiver<Box<[u8]>>;

/// A wrapper around librespot entities
pub(crate) struct Player {
    /// Connection session to Spotify
    session: Session,
    /// Inner Spotify player
    player: Arc<player::Player>,
    /// A channel to establish a separate connection between Spotify player and songbird for each track
    track_channels: flume::Sender<ByteSink>,
}

impl Player {
    pub(crate) async fn new(username: String, password: String) -> Result<Player, anyhow::Error> {
        let credentials = Credentials::with_password(username, password);
        let session = Session::new(SessionConfig::default(), None);
        session
            .connect(credentials, true)
            .await
            .context("Failed to establish session with error")?;

        let (track_channels_tx, track_channels_rx) = flume::unbounded();

        let player = librespot::playback::player::Player::new(
            PlayerConfig {
                // Treat each track as a separate one in the songbird queue
                gapless: false,
                ..Default::default()
            },
            session.clone(),
            Box::new(NoOpVolume),
            move || Box::new(MediaSink::new(track_channels_rx)),
        );

        Ok(Player {
            session,
            player,
            track_channels: track_channels_tx,
        })
    }

    /// Resolves a Spotify canonical URI or URL to Spotify to a track, album or playlist
    /// Example URIs:
    /// - track - `spotify:track:6rqhFgbbKwnb9MLmUQDhG6`
    /// - album - `spotify:album:6G9fHYDCoyEErUkHrFYfs4`
    /// - playlist - `spotify:playlist:37i9dQZF1DXcBWIGoYBM5M`
    /// Example URLs:
    /// - track - `https://open.spotify.com/track/6rqhFgbbKwnb9MLmUQDhG6`
    /// - album - `https://open.spotify.com/album/6G9fHYDCoyEErUkHrFYfs4`
    /// - playlist - `https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M`
    pub(crate) async fn resolve(&self, query: &str) -> Option<SmallVec<[Track; 1]>> {
        let id = parse_spotify_id(query)?;

        let begin = std::time::Instant::now();
        let tracks: SmallVec<[_; 1]> = match id.item_type {
            SpotifyItemType::Track => smallvec![id],
            SpotifyItemType::Album => {
                let album = metadata::Album::get(&self.session, &id).await.unwrap();
                album.tracks().cloned().collect()
            }
            SpotifyItemType::Playlist => {
                let playlist = metadata::Playlist::get(&self.session, &id).await.unwrap();
                playlist.tracks().cloned().collect()
            }
            _ => Default::default(),
        };

        let tracks = stream::iter(tracks)
            .map(|id| async move { metadata::Track::get(&self.session, &id).await })
            .buffered(16)
            .filter_map(|result| async { result.ok() })
            .map(|track| Track {
                id: track.id,
                player: self.player.clone(),
                track_channels: self.track_channels.clone(),
                metadata: extract_aux_metadata(&track),
            })
            .collect::<SmallVec<_>>()
            .await;
        info!(
            "Resolved {id} into {} tracks in {}ms",
            tracks.len(),
            begin.elapsed().as_millis()
        );

        Some(tracks)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.player.stop();
        // Notify that we are done with this session
        self.session.shutdown();
    }
}

/// Byte stream input that receives audio packets from Spotify player.
/// To avoid a mess with multiple tracks, each track uses its own channel, initiated by [MediaStream::new()]
struct MediaSink {
    /// A channel to receive track channels
    track_channels: flume::Receiver<ByteSink>,
    /// Active track channel
    sink: Option<ByteSink>,
}

impl MediaSink {
    fn new(track_channels: flume::Receiver<ByteSink>) -> Self {
        Self {
            track_channels,
            sink: None,
        }
    }
}

impl audio_backend::Sink for MediaSink {
    fn start(&mut self) -> SinkResult<()> {
        match self.track_channels.try_recv() {
            Ok(channel) => {
                self.sink = Some(channel);
                Ok(())
            }
            Err(flume::TryRecvError::Empty) => Err(SinkError::ConnectionRefused(
                "MediaSink track channel should be created at the consumer's side".into(),
            )),
            Err(flume::TryRecvError::Disconnected) => Err(SinkError::NotConnected(
                "MediaSink channel was closed".into(),
            )),
        }
    }

    fn stop(&mut self) -> SinkResult<()> {
        // We should never ever fail on stop as otherwise librespot will do `exit(1)`
        if let Some(channel) = self.sink.take() {
            // Send empty packet to notify the end of the stream
            let _ = channel.send(Box::new([]));
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        let packet = match packet {
            AudioPacket::Samples(samples) => samples
                .into_iter()
                .flat_map(|sample| (sample as f32).to_le_bytes())
                .collect::<Vec<_>>(),
            AudioPacket::Raw(packet) => packet,
        };

        let Some(sink) = self.sink.as_ref() else {
            return Err(SinkError::NotConnected("invalid MediaSink state".into()));
        };
        sink.send(packet.into_boxed_slice())
            // This error might happen if the track is skipped or bot leaves the voice channel before the track ends
            .map_err(|_| SinkError::NotConnected("Corresponding MediaStream was closed".into()))
    }
}

/// Byte stream output that sends audio packets to songbird. Works in pair with [MediaSink] with
/// which it shares commnuication channel.
#[derive(Clone)]
struct MediaStream {
    /// A stream of bytes from Spotify player. None if stream was read to the end (received empty packet)
    receiver: Option<ByteStream>,
    /// Intermediate buffer to handle cases when the whole packet could not be read
    unread: Box<[u8]>,
    /// Position where previous read finished
    read_offset: usize,
}

impl MediaStream {
    /// Establishes a new connection to the MediaSink and creates a new MediaStream if possible
    fn new(track_channels: &flume::Sender<ByteSink>) -> Option<Self> {
        // Each track has its own channel to avoid messing up with packets from different tracks
        let (byte_sink, byte_stream) = flume::bounded(16);
        track_channels.send(byte_sink).ok()?;

        // Send magic header with LE u32 sample reate and channels count to pass these values to symphonia
        let mut header = Vec::with_capacity(16);
        header.extend(b"SbirdRaw");
        header.extend((librespot::playback::SAMPLE_RATE as u32).to_le_bytes());
        header.extend((librespot::playback::NUM_CHANNELS as u32).to_le_bytes());

        Some(Self {
            receiver: Some(byte_stream),
            unread: header.into_boxed_slice(),
            read_offset: 0,
        })
    }
}

impl io::Read for MediaStream {
    fn read(&mut self, buff: &mut [u8]) -> io::Result<usize> {
        let Some(receiver) = self.receiver.as_ref() else {
            return Err(io::ErrorKind::UnexpectedEof.into());
        };

        if !self.unread.is_empty() && self.read_offset == self.unread.len() {
            // Block songbird here to handle the case when the next packet is not ready yet
            self.unread = receiver.recv().unwrap_or_default();
            self.read_offset = 0;
        }

        // Empty packet is the end of the stream marker, next read should fail
        if self.unread.is_empty() {
            self.receiver = None;
            return Ok(0);
        }

        let mut bytes_read = 0;
        loop {
            let to_read = (buff.len() - bytes_read).min(self.unread.len() - self.read_offset);
            buff[bytes_read..bytes_read + to_read]
                .copy_from_slice(&self.unread[self.read_offset..self.read_offset + to_read]);
            self.read_offset += to_read;
            bytes_read += to_read;

            // Read other packets if any and if there is some space in buff
            if bytes_read < buff.len() {
                match receiver.try_recv() {
                    Ok(packet) => {
                        self.unread = packet;
                        self.read_offset = 0;
                        if self.unread.is_empty() {
                            // Next read should return Ok(0)
                            break;
                        }
                    }
                    Err(_) => break, // No pending packets in the channel
                }
            } else {
                break; // No space left
            }
        }

        Ok(bytes_read)
    }
}

impl io::Seek for MediaStream {
    fn seek(&mut self, _pos: io::SeekFrom) -> io::Result<u64> {
        unreachable!()
    }
}

impl MediaSource for MediaStream {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// A track that can be played by Spotify player
pub(crate) struct Track {
    /// Spotify ID of the track
    id: SpotifyId,
    /// Inner Spotify player
    player: Arc<player::Player>,
    /// A channel to establish a separate connection between Spotify player and songbird for each track
    track_channels: flume::Sender<ByteSink>,
    metadata: AuxMetadata,
}

impl From<Track> for Input {
    fn from(val: Track) -> Self {
        Input::Lazy(Box::new(val))
    }
}

#[async_trait]
impl Compose for Track {
    fn should_create_async(&self) -> bool {
        true
    }

    fn create(&mut self) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        Err(AudioStreamError::Unsupported)
    }

    async fn create_async(
        &mut self,
    ) -> Result<AudioStream<Box<dyn MediaSource>>, AudioStreamError> {
        // MediaStream should be created before the player starts playing the track to avoid possible race condition,
        // as the corresponding track byte channel should be created before the player starts playing the track
        let stream = MediaStream::new(&self.track_channels).ok_or(AudioStreamError::Unsupported)?;
        self.player.load(self.id, true, 0);

        Ok(AudioStream {
            input: Box::new(stream),
            hint: None,
        })
    }

    async fn aux_metadata(&mut self) -> Result<AuxMetadata, AudioStreamError> {
        Ok(self.metadata.clone())
    }
}

/// Parses Spotify URI or URL and returns SpotifyId if possible
fn parse_spotify_id(src: &str) -> Option<SpotifyId> {
    if let Some(remaining) = src.strip_prefix("https://open.spotify.com/") {
        remaining.split_once('/').and_then(|(item_type, id)| {
            // Remove query parameters if any
            let id = id.split_once('?').map_or(id, |(id, _)| id);
            let uri = format!("spotify:{}:{}", item_type, id);
            SpotifyId::from_uri(&uri).ok()
        })
    } else {
        SpotifyId::from_uri(src).ok()
    }
}

fn extract_aux_metadata(track: &metadata::Track) -> AuxMetadata {
    let source_url = track
        .id
        .to_uri()
        .unwrap()
        .replace(':', "/")
        .replace("spotify/", "https://open.spotify.com/");

    let thumbnail = track
        .album
        .covers
        .iter()
        .find(|image| image.size == ImageSize::DEFAULT)
        .or(track.album.covers.first())
        .map(|image| format!("https://i.scdn.co/image/{}", image.id));

    use itertools::Itertools;
    let artists = track.artists.iter().map(|artist| &artist.name).join(", ");

    AuxMetadata {
        title: Some(format!("{} - {}", artists, track.name)),
        source_url: Some(source_url),
        duration: Some(Duration::from_millis(track.duration as u64)),
        thumbnail,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot::playback::audio_backend::Sink;
    use pretty_assertions::assert_eq;
    use std::{env, io::Read};

    #[test]
    fn parse_spotify_id_test() {
        // Valid Spotify URIs
        assert_eq!(
            parse_spotify_id("spotify:track:6rqhFgbbKwnb9MLmUQDhG6"),
            Some(SpotifyId::from_uri("spotify:track:6rqhFgbbKwnb9MLmUQDhG6").unwrap())
        );
        assert_eq!(
            parse_spotify_id("spotify:album:6G9fHYDCoyEErUkHrFYfs4"),
            Some(SpotifyId::from_uri("spotify:album:6G9fHYDCoyEErUkHrFYfs4").unwrap())
        );
        assert_eq!(
            parse_spotify_id("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"),
            Some(SpotifyId::from_uri("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").unwrap())
        );

        // Valid Spotify URLs
        assert_eq!(
            parse_spotify_id("https://open.spotify.com/track/6rqhFgbbKwnb9MLmUQDhG6"),
            Some(SpotifyId::from_uri("spotify:track:6rqhFgbbKwnb9MLmUQDhG6").unwrap())
        );
        assert_eq!(
            parse_spotify_id("https://open.spotify.com/album/6G9fHYDCoyEErUkHrFYfs4"),
            Some(SpotifyId::from_uri("spotify:album:6G9fHYDCoyEErUkHrFYfs4").unwrap())
        );
        assert_eq!(
            parse_spotify_id("https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M"),
            Some(SpotifyId::from_uri("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").unwrap())
        );

        // Spotify URLs from "Copy link to the track" in Spotify app
        assert_eq!(
            parse_spotify_id(
                "https://open.spotify.com/track/0X0q97XtaZHwJsYiDqyxWC?si=6647b40eba1743fe"
            ),
            Some(SpotifyId::from_uri("spotify:track:0X0q97XtaZHwJsYiDqyxWC").unwrap())
        );
        assert_eq!(
            parse_spotify_id(
                "https://open.spotify.com/album/6kUgTLymqtTyWUIKbmTMyf?si=e27fa52d985644d3"
            ),
            Some(SpotifyId::from_uri("spotify:album:6kUgTLymqtTyWUIKbmTMyf").unwrap())
        );
        assert_eq!(
            parse_spotify_id(
                "https://open.spotify.com/playlist/77RvyLiqmUimojxq3vg6mY?si=db83d5eafb0643ea"
            ),
            Some(SpotifyId::from_uri("spotify:playlist:77RvyLiqmUimojxq3vg6mY").unwrap())
        );

        // Unkown Spotify type
        assert_eq!(
            parse_spotify_id("spotify:unknown_type:37i9dQZF1DXcBWIGoYBM5M"),
            Some(SpotifyId::from_uri("spotify:unknown:37i9dQZF1DXcBWIGoYBM5M").unwrap())
        );
        assert_eq!(
            parse_spotify_id("https://open.spotify.com/unknown_type/6G9fHYDCoyEErUkHrFYfs4"),
            Some(SpotifyId::from_uri("spotify:unknown:6G9fHYDCoyEErUkHrFYfs4").unwrap())
        );

        // not matched for Spotify
        assert_eq!(
            parse_spotify_id("https://www.youtube.com/watch?v=HnL5lQXuv9M"),
            None
        );
        assert_eq!(parse_spotify_id("my random raw text query"), None);
        assert_eq!(
            parse_spotify_id("schema:track:6G9fHYDCoyEErUkHrFYfs4"),
            None
        );

        // invalid Spotify URI
        assert_eq!(parse_spotify_id("spotify:track:invalid"), None);
        assert_eq!(parse_spotify_id("spotify:track:123"), None);
        assert_eq!(parse_spotify_id("spotify:album:invalid"), None);
        assert_eq!(parse_spotify_id("spotify:playlist:invalid"), None);

        // invalid Spotify URL
        assert_eq!(
            parse_spotify_id("https://open.spotify.com/track/invalid"),
            None
        );
        assert_eq!(
            parse_spotify_id("https://open.spotify.com/album/invalid"),
            None
        );
        assert_eq!(
            parse_spotify_id("https://open.spotify.com/playlist/invalid"),
            None
        );
    }

    #[test]
    fn media_sink_test() {
        let (track_channels_tx, track_channels_rx) = flume::unbounded();
        let mut sink = MediaSink::new(track_channels_rx);
        drop(track_channels_tx);
        // Sink is disconnected
        assert!(sink.start().is_err());
        // stop should always succeed as otherwise librespot will do `exit(1)`
        assert!(sink.stop().is_ok(), "Stop should always succeed");

        let (_track_channels_tx, track_channels_rx) = flume::unbounded();
        let mut sink = MediaSink::new(track_channels_rx);
        // Sink is connected, but no channel is created yet
        assert!(sink.start().is_err());
        assert!(sink.stop().is_ok(), "Stop should always succeed");

        // write error
        let (track_channels_tx, track_channels_rx) = flume::unbounded();
        let mut sink = MediaSink::new(track_channels_rx);
        let stream = MediaStream::new(&track_channels_tx).unwrap();
        assert!(sink.start().is_ok());
        assert!(sink
            .write(
                AudioPacket::Samples(vec![1.0; 16].into()),
                &mut Converter::new(None)
            )
            .is_ok());

        drop(stream);
        assert!(sink
            .write(
                AudioPacket::Samples(vec![0.0; 16].into()),
                &mut Converter::new(None)
            )
            .is_err());
        assert!(sink.stop().is_ok(), "Stop should always succeed");

        let (track_channels_tx, track_channels_rx) = flume::unbounded();
        let mut sink = MediaSink::new(track_channels_rx);
        let stream = MediaStream::new(&track_channels_tx).unwrap();
        assert!(sink.start().is_ok());
        assert!(sink
            .write(
                AudioPacket::Samples(vec![1.0; 16].into()),
                &mut Converter::new(None)
            )
            .is_ok());
        assert!(sink.stop().is_ok(), "Stop should always succeed");
        // Sink is disconnected, write should fail now
        assert!(sink
            .write(
                AudioPacket::Samples(vec![0.0; 16].into()),
                &mut Converter::new(None)
            )
            .is_err());
        drop(stream);

        // No track channel is created, create should fail
        assert!(sink.start().is_err());
        let stream = MediaStream::new(&track_channels_tx).unwrap();
        // Now we created a MediaStream, so sink should start
        assert!(sink.start().is_ok());
        drop(stream);
    }

    #[test]
    fn media_stream_test() {
        let (track_channels_tx, track_channels_rx) = flume::unbounded();
        let mut sink = MediaSink::new(track_channels_rx);
        let mut stream = MediaStream::new(&track_channels_tx).unwrap();
        let mut buf = [0; 1024];

        // No track, just header
        assert!(sink.start().is_ok());
        assert!(sink.stop().is_ok());
        assert_eq!(stream.read(&mut buf).unwrap(), 16);
        assert_eq!(&buf[..8], b"SbirdRaw");
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
        assert!(stream.read(&mut buf).is_err());

        // First sequential track
        let mut stream = MediaStream::new(&track_channels_tx).unwrap();
        sink.start().unwrap();
        sink.write(
            // remember that we send f32 samples, each is 4 bytes long
            AudioPacket::Samples(vec![1.0; 128].into()),
            &mut Converter::new(None),
        )
        .unwrap();
        sink.stop().unwrap();

        // as everything is ready at the moment of this reading, we should read all the data at once
        assert_eq!(stream.read(&mut buf).unwrap(), 16 + 128 * 4);
        assert_eq!(&buf[..8], b"SbirdRaw");

        // and then we should see the end of the stream
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
        assert!(stream.read(&mut buf).is_err());

        // Next sequential track
        let mut stream = MediaStream::new(&track_channels_tx).unwrap();
        sink.start().unwrap();
        sink.write(
            AudioPacket::Samples(vec![0.0; 64].into()),
            &mut Converter::new(None),
        )
        .unwrap();
        sink.stop().unwrap();

        assert_eq!(stream.read(&mut buf).unwrap(), 16 + 64 * 4);
        assert_eq!(&buf[..8], b"SbirdRaw");

        // and then we should see the end of the stream
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
        assert!(stream.read(&mut buf).is_err());

        // Let's check how stream read behaves when we have multiple packets
        let mut stream = MediaStream::new(&track_channels_tx).unwrap();
        assert_eq!(stream.read(&mut buf).unwrap(), 16);
        assert_eq!(&buf[..8], b"SbirdRaw");
        sink.start().unwrap();
        sink.write(
            AudioPacket::Samples(vec![0.0; 16].into()),
            &mut Converter::new(None),
        )
        .unwrap();
        sink.write(
            AudioPacket::Samples(vec![1.0; 16].into()),
            &mut Converter::new(None),
        )
        .unwrap();
        // read all the data at once
        assert_eq!(stream.read(&mut buf).unwrap(), (16 + 16) * 4);

        sink.write(
            AudioPacket::Samples(vec![1.0; 128].into()),
            &mut Converter::new(None),
        )
        // read by portions
        .unwrap();
        assert_eq!(stream.read(&mut buf[..257]).unwrap(), 257);
        assert_eq!(stream.read(&mut buf).unwrap(), 128 * 4 - 257);

        sink.stop().unwrap();
        // and then we should see the end of the stream
        assert_eq!(stream.read(&mut buf).unwrap(), 0);
        assert!(stream.read(&mut buf).is_err());

        // it was the last stream with this channel, sink should fail
        assert!(sink.start().is_err());

        // Finally, check how multiple streams work with the same sink
        let (track_channels_tx, track_channels_rx) = flume::unbounded();
        let mut sink = MediaSink::new(track_channels_rx);
        let streams = (0..10)
            .map(|i| {
                let stream = MediaStream::new(&track_channels_tx).unwrap();
                sink.start().unwrap();
                sink.write(
                    AudioPacket::Samples(vec![0.0; 16 + i].into()),
                    &mut Converter::new(None),
                )
                .unwrap();
                sink.stop().unwrap();

                stream
            })
            .collect::<Vec<_>>();

        for (i, mut stream) in streams.into_iter().enumerate() {
            assert_eq!(stream.read(&mut buf).unwrap(), 16 + (16 + i) * 4);
            assert_eq!(&buf[..8], b"SbirdRaw");
            assert_eq!(stream.read(&mut buf).unwrap(), 0);
            assert!(stream.read(&mut buf).is_err());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn player_resolve_track_test() {
        dotenv::dotenv().expect("Set up .env file for this test");
        let _ = tracing_subscriber::fmt::try_init();

        let player = Player::new(
            env::var("SPOTIFY_USERNAME").expect("Spotify username is not set"),
            env::var("SPOTIFY_PASSWORD").expect("Spotify password is not set"),
        )
        .await
        .unwrap();

        let mut tracks = player
            .resolve("spotify:track:6rqhFgbbKwnb9MLmUQDhG6")
            .await
            .unwrap();
        assert_eq!(tracks.len(), 1);

        let Input::Lazy(mut lazy) = Input::from(tracks.pop().unwrap()) else {
            assert!(false, "Expected Lazy input");
            return;
        };
        assert_eq!(lazy.should_create_async(), true);
        assert!(lazy.create().is_err());

        let mut stream = lazy.create_async().await.unwrap();
        let mut buf = [0; 1024];
        assert_eq!(stream.input.read(&mut buf).unwrap(), 16);
        assert_eq!(&buf[..8], b"SbirdRaw");

        // at least one packet should be read
        assert_eq!(stream.input.read(&mut buf).unwrap(), buf.len());

        // The next stream created via `play` + `create_async` should interrupt the previous one via empty read
        let mut tracks = player
            .resolve("spotify:track:0X0q97XtaZHwJsYiDqyxWC")
            .await
            .unwrap();
        assert_eq!(tracks.len(), 1);
        let Input::Lazy(mut lazy) = Input::from(tracks.pop().unwrap()) else {
            assert!(false, "Expected Lazy input");
            return;
        };
        let mut next_stream = lazy.create_async().await.unwrap();

        // From the moment we've created the next stream, the previous one should return 0 on read.
        // This loop return the first zero read result or error or the last read result once all tries are exhausted
        let mut delay_read_count = 256;
        let read_result = loop {
            match stream.input.read(&mut buf) {
                Ok(0) => break Ok(0),
                Ok(read) => {
                    delay_read_count -= 1;
                    if delay_read_count == 0 {
                        break Ok(read);
                    }
                }
                Err(e) => break Err(e),
            }
        };
        assert_eq!(read_result.unwrap(), 0);
        assert!(stream.input.read(&mut buf).is_err());
        drop(stream);

        // The next stream should be readable
        assert_eq!(next_stream.input.read(&mut buf).unwrap(), 16);
        assert_eq!(&buf[..8], b"SbirdRaw");
        assert_eq!(next_stream.input.read(&mut buf).unwrap(), buf.len());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn player_resolve_album_test() {
        dotenv::dotenv().expect("Set up .env file for this test");
        let _ = tracing_subscriber::fmt::try_init();

        let player = Player::new(
            env::var("SPOTIFY_USERNAME").expect("Spotify username is not set"),
            env::var("SPOTIFY_PASSWORD").expect("Spotify password is not set"),
        )
        .await
        .unwrap();

        let tracks = player
            .resolve("https://open.spotify.com/album/1bwbZJ6khPJyVpOaqgKsoZ?si=09ea457c18c54b88")
            .await
            .unwrap();
        assert!(!tracks.is_empty());
        for track in tracks {
            assert!(matches!(Input::from(track), Input::Lazy(_)));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn player_resolve_playlist_test() {
        dotenv::dotenv().expect("Set up .env file for this test");
        let _ = tracing_subscriber::fmt::try_init();

        let player = Player::new(
            env::var("SPOTIFY_USERNAME").expect("Spotify username is not set"),
            env::var("SPOTIFY_PASSWORD").expect("Spotify password is not set"),
        )
        .await
        .unwrap();

        let tracks = player
            .resolve("https://open.spotify.com/playlist/37i9dQZF1DWZqd5JICZI0u")
            .await
            .unwrap();
        assert!(!tracks.is_empty());
        for track in tracks {
            assert!(matches!(Input::from(track), Input::Lazy(_)));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn player_resolve_other() {
        dotenv::dotenv().expect("Set up .env file for this test");
        let _ = tracing_subscriber::fmt::try_init();

        let player = Player::new(
            env::var("SPOTIFY_USERNAME").expect("Spotify username is not set"),
            env::var("SPOTIFY_PASSWORD").expect("Spotify password is not set"),
        )
        .await
        .unwrap();

        let not_resolved = [
            "https://www.youtube.com/watch?v=HnL5lQXuv9M",
            "my random raw text query",
            "schema:track:6G9fHYDCoyEErUkHrFYfs4",
            "spotify:track:invalid",
            "spotify:track:123",
            "spotify:album:invalid",
            "spotify:playlist:invalid",
            "https://open.spotify.com/track/invalid",
            "https://open.spotify.com/album/invalid",
        ];
        for query in &not_resolved {
            assert!(player.resolve(query).await.is_none());
        }

        let resolved_empty = [
            "spotify:unknown:1bwbZJ6khPJyVpOaqgKsoZ",
            "spotify:local:6rqhFgbbKwnb9MLmUQDhG6",
            "https://open.spotify.com/artist/0kq4QvLGV5t1ZoE6ittrLQ",
            "spotify:artist:0kq4QvLGV5t1ZoE6ittrLQ",
            "spotify:track:0kq4QvLGV5t1ZoE6ittrLQ",
        ];

        for query in &resolved_empty {
            assert!(player.resolve(query).await.unwrap().is_empty());
        }
    }
}
