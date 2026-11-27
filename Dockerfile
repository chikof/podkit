# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.100.0-alpine3.24

FROM rust:${RUST_VERSION} AS build
WORKDIR /app

# openssl-*: webauthn-rs (passkeys) links OpenSSL; built statically for the musl image.
# bash, python3: scripts/build-ordered.sh. lld: the linker that takes its symbol order.
RUN apk add --no-cache bash clang lld musl-dev git pkgconf python3 openssl-dev openssl-libs-static
ENV OPENSSL_STATIC=1 RUSTFLAGS="-C link-arg=-fuse-ld=lld"

RUN --mount=type=bind,source=crates,target=crates \
    --mount=type=bind,source=agent,target=agent \
    --mount=type=bind,source=cli,target=cli \
    --mount=type=bind,source=server,target=server \
    --mount=type=bind,source=scripts,target=scripts \
    --mount=type=bind,source=.sqlx,target=.sqlx \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=cache,target=/app/target/ \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    --mount=type=cache,target=/usr/local/cargo/registry/ \
SQLX_OFFLINE=true scripts/build-ordered.sh --locked --release --bin server --bin podkit-agent --bin podkit-firewall && \
cp ./target/release/server /bin/podkit && \
cp ./target/release/podkit-agent /bin/podkit-agent-$(uname -m) && \
cp ./target/release/podkit-firewall /bin/podkit-firewall-$(uname -m)

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
RUN apk add --no-cache openssh-client ca-certificates curl && \
    mkdir -p /var/lib/podkit && chown appuser /var/lib/podkit
USER appuser

COPY --from=build /bin/podkit /bin/
COPY --from=build /bin/podkit-agent-* /usr/lib/podkit/agents/
COPY --from=build /bin/podkit-firewall-* /usr/lib/podkit/agents/
ENV AGENT_BINARIES_DIR=/usr/lib/podkit/agents DATA_DIR=/var/lib/podkit
EXPOSE 8080
CMD ["/bin/podkit"]
