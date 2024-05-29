FROM rust:alpine as builder

# Dependencies for some crates
RUN apk add --no-cache alpine-sdk cmake

WORKDIR /usr/src/app

# First build a dummy target to cache dependencies in a separate Docker layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() { println!("Dummy image called!"); }' > src/main.rs
RUN cargo build --release

# Now build the real target
COPY src ./src
# Update modified attribute as otherwise cargo won't rebuild it
RUN touch -a -m ./src/main.rs
RUN cargo build --release

FROM alpine as runtime
# Prospero uses yt-dlp to play music from YouTube
COPY --from=kinkard/yt-dlp-alpine:latest /usr/local/bin/yt-dlp /usr/local/bin/yt-dlp
COPY --from=builder /usr/src/app/target/release/prospero /usr/local/bin/prospero
CMD ["prospero"]
