# About

Prospero is a minimalistic Discord music bot, inspired by [aoede](https://github.com/codetheweb/aoede).

## Build and run

The following env variables (either via set or via `.env` file) sould be set:

- **DISCORD_TOKEN** - Discord bot token
- **DATA_DIR** - path where bot can store its cache

```sh
cargo run --release
```

alternatively, Docker image can be used:

```sh
# Optional, https://hub.docker.com/repository/docker/kinkard/prospero can be used instead
make docker

echo "DISCORD_TOKEN=my discord token" > .env
echo "DATA_DIR=/storage" >> .env
docker run --rm --env-file .env -v $PWD:/storage kinkard/prospero
```

## License

All code in this project is dual-licensed under either:

- [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0) ([`LICENSE-APACHE`](LICENSE-APACHE))
- [MIT license](https://opensource.org/licenses/MIT) ([`LICENSE-MIT`](LICENSE-MIT))

at your option.
