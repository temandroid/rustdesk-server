# temandroid/rustdesk-server — hbbs/hbbr with HTTP API proxy for rustdesk-api-srv.
#
# Build from this repository (patch is already applied in source):
#   docker build -t ghcr.io/temandroid/rustdesk-server:latest .
#
# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=rust:bookworm

FROM ${RUST_IMAGE} AS builder
WORKDIR /src

RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev cmake g++ ca-certificates sqlite3 \
 && rm -rf /var/lib/apt/lists/*

# sqlx query! macros need DATABASE_URL + a live sqlite schema at compile time.
ENV DATABASE_URL=sqlite://./db_v2.sqlite3
RUN sqlite3 db_v2.sqlite3 "\
create table if not exists peer (\
  guid blob primary key not null,\
  id varchar(100) not null,\
  uuid blob not null,\
  pk blob not null,\
  created_at datetime not null default(current_timestamp),\
  user blob,\
  status tinyint,\
  note varchar(300),\
  info text not null\
) without rowid;\
create unique index if not exists index_peer_id on peer (id);\
create index if not exists index_peer_user on peer (user);\
create index if not exists index_peer_created_at on peer (created_at);\
create index if not exists index_peer_status on peer (status);\
"

COPY . .
RUN cargo build --locked --release --bin hbbs --bin hbbr \
 && strip target/release/hbbs target/release/hbbr \
 && ./target/release/hbbs --help >/dev/null \
 && ./target/release/hbbr --help >/dev/null

FROM debian:bookworm-slim
LABEL org.opencontainers.image.title="temandroid/rustdesk-server" \
      org.opencontainers.image.description="RustDesk hbbs/hbbr with HTTP API proxy for rustdesk-api-srv" \
      org.opencontainers.image.source="https://github.com/temandroid/rustdesk-server" \
      org.opencontainers.image.licenses="AGPL-3.0"

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/hbbs /usr/bin/hbbs
COPY --from=builder /src/target/release/hbbr /usr/bin/hbbr

WORKDIR /root
ENV HOME=/root
# Default matches co-located API on host port 21114 when hbbs uses 21116.
ENV API_SERVER=http://127.0.0.1:21114

CMD ["hbbs"]
