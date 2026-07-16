FROM rust:1-bookworm AS builder

WORKDIR /app

RUN rustup target add wasm32-unknown-unknown \
    && apt-get update \
    && apt-get install -y --no-install-recommends cmake perl pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install trunk --locked \
    && cargo install wasm-bindgen-cli --version 0.2.125 --locked

COPY Cargo.toml Cargo.lock ./
COPY server ./server
COPY frontend ./frontend

RUN cargo build --release -p nivrhone-server \
    && cd frontend \
    && trunk build index.html --dist dist --release

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /app --shell /usr/sbin/nologin rhonometre \
    && mkdir -p /app /data/programmes \
    && chown -R rhonometre:rhonometre /app /data

WORKDIR /app

COPY --from=builder --chown=rhonometre:rhonometre /app/target/release/nivrhone-server /usr/local/bin/rhonometre-server
COPY --from=builder --chown=rhonometre:rhonometre /app/frontend/dist /app/dist

ENV HOST=0.0.0.0
ENV PORT=8080
ENV STATIC_DIR=/app/dist
ENV RHONOMETRE_PROGRAMME_DIR=/data/programmes

EXPOSE 8080

USER rhonometre

CMD ["rhonometre-server"]
