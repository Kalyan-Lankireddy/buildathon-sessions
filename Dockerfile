# Shopping Sessions: native Rust pipeline, multi-stage build.
# Builder and runtime are pinned by tag AND digest (the multi-arch index).
# The scorers are linux/amd64 and build this file natively, so no platform is pinned here.

# ---- builder: Rust 1.100.0 on Debian bookworm
FROM rust:1.100.0-slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS builder
WORKDIR /src
# x86-64-v3 = AVX2, safe on every Cascade Lake or newer Xeon.
ENV RUSTFLAGS="-C target-cpu=x86-64-v3" \
    CARGO_TERM_COLOR=never
# Dependencies first, so a source-only change does not rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY src/ src/
# COPY keeps the files' old mtimes; touch them so cargo rebuilds our crate, not just the cached stub.
RUN touch src/*.rs && cargo build --release --locked

# ---- runtime: one static-ish binary on a small Debian
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
COPY --from=builder /src/target/release/sessions /app/sessions
# At run time the root fs is read-only, the user is 1000:1000, there is no network,
# /data is read-only, and /output and /scratch are the only writable places.
WORKDIR /app
CMD ["/app/sessions"]
