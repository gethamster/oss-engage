FROM rust:1.98-bookworm AS build
WORKDIR /src
ARG ENGAGE_REVISION=unversioned-source
ENV ENGAGE_BUILD_REVISION=$ENGAGE_REVISION
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN rustup target add wasm32-unknown-unknown \
    && cargo install trunk --version 0.22.2 --locked
RUN cd crates/engage-inspector && trunk build --release --public-url /inspect/
RUN cargo build --locked --release -p engage-cli \
    && python3 scripts/package.py --output /release \
    && mv /release/engage-*-unknown-linux-gnu /release/package

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home engage
COPY --from=build /release/package/ /app/
USER engage
WORKDIR /home/engage
EXPOSE 4100
ENTRYPOINT ["/app/engage", "serve"]
CMD ["--bind", "0.0.0.0:4100", "--inspector-dir", "/app/inspector"]
