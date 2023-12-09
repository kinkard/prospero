use librespot::connect::spirc::Spirc;
use librespot::core::cache::Cache;
use librespot::core::{
    authentication::Credentials,
    config::{ConnectConfig, DeviceType, SessionConfig},
    session::Session,
};
use librespot::playback::{
    audio_backend,
    audio_backend::SinkResult,
    config::Bitrate,
    config::{PlayerConfig, VolumeCtrl},
    convert::Converter,
    decoder::AudioPacket,
    mixer::softmixer::SoftMixer,
    mixer::{Mixer, MixerConfig},
    player::Player,
};

use serenity::client::Context;
use serenity::prelude::TypeMapKey;
use songbird::input::core::io::MediaSource;
use songbird::input::{AudioStream, Input, LiveInput};

use std::clone::Clone;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use byteorder::{ByteOrder, LittleEndian};

/// Key to store SpotifyPlayer in the serenity context
pub(crate) struct SpotifyPlayerKey;

impl TypeMapKey for SpotifyPlayerKey {
    type Value = Arc<SpotifyPlayer>;
}

pub(crate) async fn get(ctx: &Context) -> Option<Arc<SpotifyPlayer>> {
    let data = ctx.data.read().await;
    data.get::<SpotifyPlayerKey>().cloned()
}

/// A wrapper around librespot entities
pub(crate) struct SpotifyPlayer {
    /// Connection session to Spotify
    session: Session,
    /// Object to control player, e.g. spirc.shutdown()
    spirc: Spirc,
    /// Audio stream that should be readed by the discord voice channel
    media_stream: MediaStream,
}

impl SpotifyPlayer {
    pub(crate) async fn new(
        username: String,
        password: Option<String>,
        cache_location: Option<String>,
    ) -> Result<SpotifyPlayer, String> {
        let cache = if let Some(cache_location) = cache_location {
            // Store caches for different usernames in separate subfolders
            let mut user_cache_location = PathBuf::from(cache_location);
            user_cache_location.push(&username);

            let cache = Cache::new(
                Some(&user_cache_location),
                Some(&user_cache_location),
                // todo: Cache audio files and limit overall cache size
                None,
                None,
            )
            .map_err(|err| format!("Failed to create cache due to '{err}'"))?;
            Some(cache)
        } else {
            None
        };

        let credentials = password
            .map(|password| Credentials::with_password(username, password))
            .or_else(|| cache.as_ref().and_then(|cache| cache.credentials()))
            .ok_or(String::from("Password not provided and not cached"))?;

        let (session, _) = Session::connect(SessionConfig::default(), credentials, cache, true)
            .await
            .map_err(|err| format!("Failed to establish session with error {err:?}"))?;

        let mixer = Box::new(SoftMixer::open(MixerConfig {
            volume_ctrl: VolumeCtrl::Linear,
            ..MixerConfig::default()
        }));

        let (media_sink, media_stream) = create_media_channel();

        let (player, event_channel) = Player::new(
            PlayerConfig {
                // Anyway discord reduces bitrate to 96k, so there is no point to pull more data
                bitrate: Bitrate::Bitrate96,
                ..Default::default()
            },
            session.clone(),
            mixer.get_soft_volume(),
            move || Box::new(media_sink),
        );
        // Just drop it as we don't need player events for now
        drop(event_channel);

        let spirc_config = ConnectConfig {
            name: "Prospero".to_string(),
            device_type: DeviceType::AudioDongle,
            initial_volume: None,
            has_volume_ctrl: true,
            autoplay: false,
        };
        let (spirc, task) = Spirc::new(spirc_config, session.clone(), player, mixer);

        // Task that processes communication with Spotify control device like desktop, mobile or web UI.
        // It will shutdown once `spirc.shutdown()` is called.
        tokio::spawn(async {
            task.await;
        });

        Ok(SpotifyPlayer {
            session,
            spirc,
            media_stream,
        })
    }

    pub(crate) fn audio_input(&self) -> Input {
        // Basically we do what songbird does in `RawAdapter` but in much more simpler way,
        // as we can simply put the magic header `b"SbirdRaw\0\0\0\0\0\0\0\0"` + LE u32 sample rate and channels count
        // directly to the channel. See `create_media_channel()` for more details.
        Input::Live(
            LiveInput::Raw(AudioStream {
                input: Box::new(self.media_stream.clone()),
                hint: None,
            }),
            None,
        )
    }

    pub(crate) fn play(&self) {
        self.spirc.play();
    }

    pub(crate) fn pause(&self) {
        self.spirc.pause();
    }
}

impl Drop for SpotifyPlayer {
    fn drop(&mut self) {
        // Notify that we are done with this session
        self.session.shutdown();
        // Stop task we've created in the `new()`
        self.spirc.shutdown();
    }
}

struct MediaSink(flume::Sender<Vec<u8>>);

#[derive(Clone)]
struct MediaStream {
    receiver: flume::Receiver<Vec<u8>>,
    /// Intermediate buffer to handle cases when the whole packet could not be read
    unread: Vec<u8>,
    /// Position where previous read finished
    read_offset: usize,
}

fn create_media_channel() -> (MediaSink, MediaStream) {
    let (sender, receiver) = flume::bounded::<Vec<u8>>(16);

    // Send magic header with LE u32 sample reate and channels count to pass these values to symphonia
    let mut header = vec![0_u8; 16];
    header[..8].copy_from_slice(b"SbirdRaw");
    LittleEndian::write_u32(&mut header[8..12], librespot::playback::SAMPLE_RATE);
    LittleEndian::write_u32(&mut header[12..], 2); // channels count

    (
        MediaSink(sender),
        MediaStream {
            receiver,
            unread: header,
            read_offset: 0,
        },
    )
}

impl audio_backend::Sink for MediaSink {
    fn start(&mut self) -> SinkResult<()> {
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else {
            unreachable!("librespot uses only f64 samples");
        };

        // todo: we can reuse the memory we have
        let packet = samples
            .into_iter()
            .map(|sample| {
                let mut buff: [u8; 4] = Default::default();
                LittleEndian::write_f32(&mut buff, sample as f32);
                buff
            })
            .flatten()
            .collect::<Vec<_>>();
        self.0.send(packet).unwrap();
        Ok(())
    }
}

impl io::Read for MediaStream {
    fn read(&mut self, buff: &mut [u8]) -> io::Result<usize> {
        if self.unread.is_empty() || self.read_offset == self.unread.len() {
            // Block songbird here instead of returning 0 to avoid switching to the next track,
            // as we handle spotify as an infinite one
            self.unread = self.receiver.recv().unwrap();
            self.read_offset = 0;
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
                match self.receiver.try_recv() {
                    Ok(packet) => {
                        self.unread = packet;
                        self.read_offset = 0;
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
