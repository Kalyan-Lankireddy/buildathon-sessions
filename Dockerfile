# Shopping Sessions: native Rust pipeline, multi-stage build.
# Builder and runtime are pinned by tag AND digest (the multi-arch index).
# The scorers are linux/amd64 and build this file natively, so no platform is pinned here.

# ---- builder: Rust 1.100.0 on Debian bookworm
FROM rust:1.100.0-slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS builder
WORKDIR /src
# cascadelake = AVX-512 (VL/BW/DQ/CD/VNNI); the scorer is Cascade Lake or newer. Not runnable on a Mac (Rosetta has no AVX-512), so this build is checked by the scorer, not locally.
# (the arm64 build used for quick Linux timing loops on a Mac passes --build-arg RUSTFLAGS="")
ARG RUSTFLAGS="-C target-cpu=cascadelake"
# An explicit --target makes RUSTFLAGS apply to our binary only, not to build scripts, which run
# on the builder and must not need AVX-512 (the arm64 timing build passes aarch64-unknown-linux-gnu).
ARG CARGO_TARGET=x86_64-unknown-linux-gnu
ENV RUSTFLAGS=$RUSTFLAGS \
    CARGO_TERM_COLOR=never
# Dependencies first, so a source-only change does not rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked --target $CARGO_TARGET \
    && rm -rf src
COPY src/ src/
# COPY keeps the files' old mtimes; touch them so cargo rebuilds our crate, not just the cached stub.
RUN touch src/*.rs && cargo build --release --locked --target $CARGO_TARGET \
    && cp target/$CARGO_TARGET/release/sessions /sessions

# ---- runtime: one static-ish binary on a small Debian
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
COPY --from=builder /sessions /app/sessions
# At run time the root fs is read-only, the user is 1000:1000, there is no network,
# /data is read-only, and /output and /scratch are the only writable places.
WORKDIR /app
CMD ["/app/sessions"]
