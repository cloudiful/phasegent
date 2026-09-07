# syntax=docker/dockerfile:1

# phasegent MCP container: CLI-only runtime-only image.
#
# - CI builds the CLI per-arch (`cargo build --release --bin phasegent
#   --no-default-features`, no `gui` feature) and stages the binary at
#   `ci-image-input/phasegent`; this Dockerfile only copies that prebuilt
#   artifact, so no Rust toolchain or `cargo build` runs inside Docker.
# - Runtime is a minimal Debian slim image running as a non-root user.
# - Default command serves authenticated MCP over streamable HTTP on
#   loopback (`127.0.0.1:3000`); stdio stays available via an explicit
#   command override and stdout stays protocol-clean (exec-form
#   ENTRYPOINT, diagnostics go to stderr).
# - Persistent state lives under the `/data` volume; override with
#   PHASEGENT_DB_PATH / PHASEGENT_CONFIG_PATH. Secrets are never baked
#   into the image: pass PHASEGENT_MCP_AUTH_TOKEN with `-e`.
# - Per-arch correctness is enforced by the image pipeline (each arch
#   image copies the matching arch binary); no cross-arch reuse.

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

# Prebuilt CLI artifact staged by CI per-arch (see .dockerignore: this
# path is never excluded from the build context).
COPY --chmod=755 ci-image-input/phasegent /usr/local/bin/phasegent

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
