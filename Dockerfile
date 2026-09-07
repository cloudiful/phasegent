# syntax=docker/dockerfile:1

# phasegent MCP container: CLI-only multi-stage image.
#
# - Builder compiles the CLI only (`cargo build --release --bin phasegent`
#   without the `gui` feature), so no Tauri/GUI system libraries or
#   frontend build are required.
# - Runtime is a minimal Debian slim image running as a non-root user.
# - Default command serves authenticated MCP over streamable HTTP on
#   loopback (`127.0.0.1:3000`); stdio stays available via an explicit
#   command override and stdout stays protocol-clean (exec-form
#   ENTRYPOINT, diagnostics go to stderr).
# - Persistent state lives under the `/data` volume; override with
#   PHASEGENT_DB_PATH / PHASEGENT_CONFIG_PATH. Secrets are never baked
#   into the image: pass PHASEGENT_MCP_AUTH_TOKEN with `-e`.

FROM docker.io/library/rust:1-bookworm AS builder
WORKDIR /app

# Dependency manifest first for better layer caching, then the CLI
# sources. No frontend, no Tauri bundle, no lockfile required.
COPY Cargo.toml ./
COPY build.rs ./
COPY src ./src
COPY migrations ./migrations

# CLI-only release build. Default features are empty (no `gui`); the
# explicit `--no-default-features` pins that even if defaults change.
# The exact `cargo build --release --bin phasegent` contract is asserted
# by tests/container_contract.rs.
RUN cargo build --release --bin phasegent --no-default-features

FROM docker.io/library/debian:bookworm-slim

# Minimal runtime: CA certificates for provider HTTPS only. No GUI
# libraries, no build toolchain, no shell wrappers around the binary.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 65532 --gid nogroup \
        --home /nonexistent --no-create-home \
        --shell /usr/sbin/nologin phasegent \
    && mkdir -p /data \
    && chown 65532:nogroup /data

COPY --from=builder /app/target/release/phasegent /usr/local/bin/phasegent

# Persistent SQLite/config storage. Mount a named volume or host dir at
# /data; PHASEGENT_DB_PATH / PHASEGENT_CONFIG_PATH may point elsewhere
# and are passed through verbatim when set with `docker run -e`.
VOLUME /data
ENV PHASEGENT_DB_PATH=/data/phasegent.sqlite3 \
    PHASEGENT_CONFIG_PATH=/data/phasegent.toml

EXPOSE 3000

USER 65532:nogroup

# Deterministic exec-form entrypoint: no shell, so stdio stdout stays
# protocol-clean. The default serves authenticated HTTP MCP on loopback
# and fails closed without PHASEGENT_MCP_AUTH_TOKEN.
ENTRYPOINT ["/usr/local/bin/phasegent"]
CMD ["--role", "executor", "mcp", "serve", "--transport", "http", "--bind", "127.0.0.1:3000"]
