# About

Features:

- Play music from Spotify (requires a Spotify Premium account).
- Generate a brief overview of what people talked about using automatic speech recognition from OpenAI's Whisper and ChatGPT (to be implemented).

## Build and run

The following env variables (either via set or via `.env` file) sould be set:

- DISCORD_TOKEN
- SPOTIFY_USERNAME
- SPOTIFY_PASSWD

```sh
cargo run --release
```

## License

All code in this project is dual-licensed under either:

- [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0) ([`LICENSE-APACHE`](LICENSE-APACHE))
- [MIT license](https://opensource.org/licenses/MIT) ([`LICENSE-MIT`](LICENSE-MIT))

at your option.
This means you can select the license you prefer!
This dual-licensing approach is the de-facto standard in the Rust ecosystem and there are [very good reasons](https://github.com/bevyengine/bevy/issues/2373) to include both.
