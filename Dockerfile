FROM rust:alpine3.22 AS builder

# Dependencies for some crates
RUN apk add --no-cache alpine-sdk cmake python3-dev

# Embedded Python loads stdlib C modules via dlopen, which a static musl binary can't do
ENV RUSTFLAGS="-C target-feature=-crt-static"

WORKDIR /usr/src/app

# First build a dummy target to cache dependencies in a separate Docker layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() { println!("Dummy image called!"); }' > src/main.rs
RUN cargo build --release

# Now build the real target
COPY src ./src
RUN touch -a -m ./src/main.rs
RUN cargo build --release

FROM alpine:3.22 AS runtime
# yt-dlp is used as a library by the resolver process, so a real Python is needed.
RUN apk add --no-cache python3 \
  && apk add --no-cache --virtual .pip py3-pip \
  && pip install --no-cache-dir --break-system-packages "yt-dlp==2026.8.19" \
  && apk del .pip \
  && rm -rf /root/.cache \
  && cd /usr/lib/python3.12 \
  # Drop dev tooling and modules (with their native libs) that the embedded interpreter never touches
  && rm -rf ensurepip idlelib lib2to3 pydoc_data turtledemo unittest venv tkinter config-3.12-* \
    turtle.py pydoc.py curses sqlite3 \
    lib-dynload/_test* lib-dynload/_ctypes_test* lib-dynload/xx* lib-dynload/_xx* \
    lib-dynload/_curses* lib-dynload/readline* lib-dynload/_sqlite3* \
  && rm -rf /usr/lib/libstdc++.so* /usr/lib/libmpdec++.so* /usr/lib/libsqlite3.so* \
    /usr/lib/libncursesw.so* /usr/lib/libpanelw.so* /usr/lib/libreadline.so* /usr/lib/libhistory.so* \
    /etc/terminfo /usr/share/terminfo \
  # Ship bytecode only: site-packages is read-only at runtime, and .py + __pycache__ doubles the size
  && python3 -m compileall -q -b -j 0 . \
  && find . -name '*.py' -delete \
  && find . -name __pycache__ -type d -prune -exec rm -rf {} +
COPY --from=builder /usr/src/app/target/release/prospero /usr/local/bin/prospero
CMD ["prospero"]
