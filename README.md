# About

Prospero is a minimalistic Spotify Discord bot, inspired by [aoede](https://github.com/codetheweb/aoede).

Once launched and connected to the Discord guild, Prospero can be set up with Spotify account to play from (Spotify Premium is required) via `/connect_spotify` discord command. Credentials for this command can be obtained here - <https://www.spotify.com/us/account/set-device-password/>. After that Prospero will appear as a device and can be controled remotely via Spotify desktop/mobile/web app.

## Build and run

The following env variables (either via set or via `.env` file) sould be set:

- **DISCORD_TOKEN**
- **DATA_DIR** - path to the folder where Prospero can store data

```sh
cargo run --release
```

alternatively, Docker image can be used:

```sh
echo "DISCORD_TOKEN=my discord token" > .env
echo "DATA_DIR=/storage" >> .env
docker run --rm --env-file .env -v $PWD:/storage kinkard/prospero
```

## License

All code in this project is dual-licensed under either:

- [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0) ([`LICENSE-APACHE`](LICENSE-APACHE))
- [MIT license](https://opensource.org/licenses/MIT) ([`LICENSE-MIT`](LICENSE-MIT))

at your option.
This means you can select the license you prefer!
This dual-licensing approach is the de-facto standard in the Rust ecosystem and there are [very good reasons](https://github.com/bevyengine/bevy/issues/2373) to include both.
