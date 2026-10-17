# Dedicated server + master server image.
#   docker build -f deploy/Dockerfile -t kotor-coop .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p kotor-server -p kotor-master

FROM debian:bookworm-slim
RUN useradd --system --home /data kotor && mkdir -p /data && chown kotor /data
COPY --from=build /src/target/release/kotor-server /src/target/release/kotor-master /usr/local/bin/
USER kotor
WORKDIR /data
EXPOSE 27700/tcp 27780/tcp
# The server writes a default server.toml on first start; edit it in the volume.
CMD ["kotor-server", "--config", "/data/server.toml", "--data-dir", "/data/world", "--no-console"]
