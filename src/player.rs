use librespot::connect::spirc::Spirc;
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
use songbird::input::{self, Input};

use std::clone::Clone;
use std::sync::Arc;
use std::{io, mem};

use byteorder::{ByteOrder, LittleEndian};
use rubato::{FftFixedInOut, Resampler};
use songbird::input::reader::MediaSource;

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
    pub(crate) async fn new(username: String, password: String) -> Result<SpotifyPlayer, String> {
        let (session, _) = Session::connect(
            SessionConfig::default(),
            Credentials::with_password(username, password),
            None, // todo: add a cache for audio files with some reasonable limit
            false,
        )
        .await
        .map_err(|err| format!("Failed to establish session with error {err:?}"))?;

        let mixer = Box::new(SoftMixer::open(MixerConfig {
            volume_ctrl: VolumeCtrl::Linear,
            ..MixerConfig::default()
        }));

        let (media_sink, media_stream) = create_media_channel();

        let (player, event_channel) = Player::new(
            PlayerConfig {
                bitrate: Bitrate::Bitrate320,
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

    pub(crate) fn audio_source(&self) -> Input {
        let mut decoder = input::codec::OpusDecoderState::new().unwrap();
        decoder.allow_passthrough = false;

        input::Input::new(
            true,
            input::reader::Reader::Extension(Box::new(self.media_stream.clone())),
            input::codec::Codec::FloatPcm,
            input::Container::Raw,
            None,
        )
    }

    pub(crate) fn stop(&self) {
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

struct MediaSink {
    /// Resampler to convert from spotify sample rate to discord
    resampler: FftFixedInOut<f32>,
    /// Number of frames in each chunk to process
    resampler_chunk_size: usize,
    /// Input buffer for resampler where we collect frames
    input_buffer: (Vec<f32>, Vec<f32>),
    /// Output buffer for resampler
    output_buffer: Vec<Vec<f32>>,
    /// Channel for resampled frames
    sender: flume::Sender<[f32; 2]>,
}

#[derive(Clone)]
struct MediaStream(flume::Receiver<[f32; 2]>);

fn create_media_channel() -> (MediaSink, MediaStream) {
    let resampler = FftFixedInOut::<f32>::new(
        librespot::playback::SAMPLE_RATE as usize,
        songbird::constants::SAMPLE_RATE_RAW,
        1024,
        2,
    )
    .unwrap();

    // Bound channel to the single chunk to simplify synchronizations between Sink and Stream
    let (sender, receiver) = flume::bounded::<[f32; 2]>(resampler.output_frames_max());
    let resampler_chunk_size = resampler.input_frames_max();

    (
        MediaSink {
            resampler_chunk_size,
            input_buffer: (
                Vec::with_capacity(resampler_chunk_size),
                Vec::with_capacity(resampler_chunk_size),
            ),
            output_buffer: resampler.output_buffer_allocate(),
            resampler,
            sender,
        },
        MediaStream(receiver),
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
        for c in packet.samples().unwrap().chunks_exact(2) {
            self.input_buffer.0.push(c[0] as f32);
            self.input_buffer.1.push(c[1] as f32);
            if self.input_buffer.0.len() == self.resampler_chunk_size {
                self.resampler
                    .process_into_buffer(
                        &[
                            &self.input_buffer.0[0..self.resampler_chunk_size],
                            &self.input_buffer.1[0..self.resampler_chunk_size],
                        ],
                        &mut self.output_buffer,
                        None,
                    )
                    .unwrap();

                self.input_buffer.0.clear();
                self.input_buffer.1.clear();

                let sender = self.sender.clone();

                for i in 0..self.output_buffer[0].len() {
                    sender
                        .send([self.output_buffer[0][i], self.output_buffer[1][i]])
                        .unwrap()
                }
            }
        }

        Ok(())
    }
}

impl io::Read for MediaStream {
    fn read(&mut self, buff: &mut [u8]) -> io::Result<usize> {
        let sample_size = mem::size_of::<f32>() * 2;

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
                LittleEndian::write_f32_into(
                    &sample,
                    &mut buff[bytes_written..(bytes_written + sample_size)],
                );
            } else if let Ok(data) = self.0.try_recv() {
                LittleEndian::write_f32_into(
                    &data,
                    &mut buff[bytes_written..(bytes_written + sample_size)],
                );
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
