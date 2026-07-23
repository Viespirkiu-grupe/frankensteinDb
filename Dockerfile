# syntax=docker/dockerfile:1.7

FROM rust:1.88-alpine3.21 AS builder

WORKDIR /build
RUN apk add --no-cache make musl-dev

COPY Cargo.toml Cargo.lock ./
RUN --mount=type=cache,id=frankensteindb-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=frankensteindb-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo fetch --locked

COPY src ./src
COPY docs/openapi.json ./docs/openapi.json

ARG BUILD_PROFILE=release
RUN --mount=type=cache,id=frankensteindb-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=frankensteindb-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=frankensteindb-release-target,target=/build/target,sharing=locked \
    cargo build --locked --profile "${BUILD_PROFILE}" \
      --bin frankensteindb \
      --bin frankensteindb-server \
    && cp "target/${BUILD_PROFILE}/frankensteindb" /tmp/frankensteindb \
    && cp "target/${BUILD_PROFILE}/frankensteindb-server" /tmp/frankensteindb-server

FROM alpine:3.21 AS runtime

ARG VERSION=dev
ARG VCS_REF=unknown
ARG BUILD_DATE=unknown

LABEL org.opencontainers.image.title="FrankensteinDB" \
      org.opencontainers.image.description="Search-first database backed by SQLite and Tantivy" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.licenses="AGPL-3.0-only"

RUN apk add --no-cache ca-certificates tini \
    && addgroup -S -g 10001 frankensteindb \
    && adduser -S -D -H -u 10001 -G frankensteindb frankensteindb \
    && install -d -o frankensteindb -g frankensteindb /data

COPY --from=builder /tmp/frankensteindb /usr/local/bin/frankensteindb
COPY --from=builder /tmp/frankensteindb-server /usr/local/bin/frankensteindb-server

USER frankensteindb
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=5 \
  CMD wget -q -O /dev/null http://127.0.0.1:8080/health || exit 1

ENTRYPOINT ["/sbin/tini", "--"]
CMD ["frankensteindb-server", "/data", "--listen", "0.0.0.0:8080"]
