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
RUN cargo install --path .

# Songbird uses yt-dlp to play music from http(s) link. Unfortunately, `apk add yt-dlp` adds
# too much because of python3 and ffmpeg dependencies, and standalone binaries are built not for
# musl (alpine linker). So we build our own standalone binary to cut docker image size.
# The `pyinstaller-alpine` should be built before using the following command:
# `docker build https://github.com/pyinstaller/pyinstaller.git -f alpine.dockerfile -t pyinstaller-alpine`
FROM pyinstaller-alpine as yt-dlp

# Unfortunately, we can't pass our Dockerfile to the yt-dlp repo context,
# and `ADD` just doesn't work, so nothing else we can do except git clone
WORKDIR /usr/src
RUN apk add --no-cache git
RUN git clone https://github.com/yt-dlp/yt-dlp.git
WORKDIR /usr/src/yt-dlp

RUN python3 -m pip install -U pyinstaller -r requirements.txt
RUN python3 devscripts/make_lazy_extractors.py
RUN python3 pyinst.py

FROM alpine as runtime
COPY --from=yt-dlp /usr/src/yt-dlp/dist/yt-dlp_linux_aarch64 /usr/local/bin/yt-dlp
COPY --from=builder /usr/local/cargo/bin/prospero /usr/local/bin/prospero
CMD ["prospero"]
