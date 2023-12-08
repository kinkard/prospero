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
use std::path::PathBuf;
use std::sync::Arc;
use std::{io, mem};

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

type AudioSample = [u8; 8];

struct MediaSink(flume::Sender<AudioSample>);

#[derive(Clone)]
struct MediaStream(flume::Receiver<AudioSample>);

fn create_media_channel() -> (MediaSink, MediaStream) {
    let (sender, receiver) = flume::bounded::<AudioSample>(1024);

    // Send magic header with LE u32 sample reate and channels count to pass these values to symphonia
    sender.send(*b"SbirdRaw").unwrap();

    let mut header: AudioSample = Default::default();
    LittleEndian::write_u32_into(&[librespot::playback::SAMPLE_RATE, 2], &mut header);
    sender.send(header).unwrap();

    (MediaSink(sender), MediaStream(receiver))
}

impl audio_backend::Sink for MediaSink {
    fn start(&mut self) -> SinkResult<()> {
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        for c in packet.samples().unwrap().chunks_exact(2) {
            let mut sample: [u8; 8] = Default::default();
            LittleEndian::write_f32_into(&[c[0] as f32, c[1] as f32], &mut sample);
            self.0.send(sample).unwrap();
        }
        Ok(())
    }
}

impl io::Read for MediaStream {
    fn read(&mut self, buff: &mut [u8]) -> io::Result<usize> {
        let sample_size = mem::size_of::<AudioSample>();

        if buff.len() < sample_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MediaStream does not support read buffer too small to guarantee \
                holding one audio sample (8 bytes)",
            ));
        }

        let mut bytes_written = 0;
        while bytes_written + (sample_size - 1) < buff.len() {
            if bytes_written == 0 {
                // We can not return 0 bytes because songbird then thinks that the track has ended,
                // therefore block until at least one stereo data set can be returned.

                let sample = self.0.recv().unwrap();
                buff[bytes_written..(bytes_written + sample_size)].copy_from_slice(&sample);
            } else if let Ok(sample) = self.0.try_recv() {
                buff[bytes_written..(bytes_written + sample_size)].copy_from_slice(&sample);
            } else {
                break;
            }
            bytes_written += sample_size;
        }

        Ok(bytes_written)
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
