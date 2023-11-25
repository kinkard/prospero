FROM rust:alpine as builder

# Dependencies for audiopus_sys, used by librespot-audio
RUN apk add --no-cache alpine-sdk cmake automake autoconf opus libtool

WORKDIR /usr/src/app

# First build a dummy target to cache dependencies in a separate Docker layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() { println!("Dummy image called!"); }' > src/main.rs
RUN cargo build --release

# Now build the real target
COPY src ./src
# Update modified attribute as otherwise cargo won't rebuild it
RUN touch -a -m ./src/main.rs
RUN cargo install --path .

FROM alpine as runtime
COPY --from=builder /usr/local/cargo/bin/prospero /usr/local/bin/prospero
CMD ["prospero"]
