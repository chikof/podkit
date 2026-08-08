# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.97-alpine3.21
ARG APP_NAME=server

FROM rust:${RUST_VERSION} AS build
ARG APP_NAME
WORKDIR /app

RUN apk add --no-cache clang lld musl-dev git

RUN --mount=type=bind,source=crates,target=crates \
    --mount=type=bind,source=server,target=server \
    --mount=type=bind,source=.sqlx,target=.sqlx \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=cache,target=/app/target/ \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/usr/local/cargo/registry/ \
SQLX_OFFLINE=true cargo build --locked --release && \
cp ./target/release/$APP_NAME /bin/podkit

FROM alpine:3.24 AS final

ARG UID=10001
RUN adduser \
    --disabled-password \
    --gecos "" \
    --home "/nonexistent" \
    --shell "/sbin/nologin" \
    --no-create-home \
    --uid "${UID}" \
    appuser
USER appuser

COPY --from=build /bin/podkit /bin/
EXPOSE 8080
CMD ["/bin/podkit"]
