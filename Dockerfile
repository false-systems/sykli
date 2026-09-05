# syntax=docker/dockerfile:1.7
# Two images from one static binary.
#
#   docker build -t sykli .                         # scratch: the binary, nothing else
#   docker build --target tools -t sykli:tools .    # debian-slim + git + jq, for CI
#
# sykli binds every receipt to the Git tree it evaluated, so it needs `git`
# next to the repository. The scratch image has none: use it where the host
# already provides git on a mounted checkout, or use `tools`.
#
# Cross-compiles with musl from the build platform, so a multi-arch push does
# not emulate the compiler: docker buildx build --platform linux/amd64,linux/arm64.

FROM --platform=$BUILDPLATFORM rust:1-slim-bookworm AS builder
ARG TARGETARCH
RUN set -eu; \
    apt-get update; \
    apt-get install -y --no-install-recommends musl-tools; \
    case "$(uname -m):$TARGETARCH" in \
      x86_64:arm64) apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu ;; \
      aarch64:amd64) apt-get install -y --no-install-recommends gcc-x86-64-linux-gnu ;; \
    esac; \
    rm -rf /var/lib/apt/lists/*
WORKDIR /src
# The whole workspace: Cargo refuses to build with a member missing, and the
# member list grows (xtask, mcp). .dockerignore keeps the context small.
COPY . .
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    rustup target add "$target"; \
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc; \
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-gnu-gcc; \
    case "$(uname -m):$TARGETARCH" in \
      x86_64:amd64|aarch64:arm64) unset CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER ;; \
    esac; \
    cargo build --release --locked --target "$target"; \
    mkdir -p /out; \
    install -m 0755 "target/$target/release/sykli" /out/sykli; \
    /out/sykli --version

# CI image: git for the tree OID, jq for reading receipts, CA roots for fetches.
FROM debian:bookworm-slim AS tools
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates jq \
 && rm -rf /var/lib/apt/lists/* \
 && git config --system --add safe.directory '*'
COPY --from=builder /out/sykli /usr/local/bin/sykli
WORKDIR /repo
ENTRYPOINT ["sykli"]

# Default: the binary alone. `sykli --help` works; receipts need git on a
# mounted repository or the `tools` target.
FROM scratch AS sykli
COPY --from=builder /out/sykli /usr/local/bin/sykli
ENTRYPOINT ["/usr/local/bin/sykli"]
