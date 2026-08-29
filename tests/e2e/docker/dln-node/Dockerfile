# syntax=docker/dockerfile:1

# Pre-built binary approach: the host builds dln-node and provides
# the binary via the build context, avoiding path dependency issues.
# Build with:  docker build -f ... --build-arg BINARY=target/debug/dln-node .

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --create-home --home-dir /var/lib/dln-node dln-node
WORKDIR /var/lib/dln-node

ARG BINARY=target/debug/dln-node
COPY ${BINARY} /usr/local/bin/dln-node

USER dln-node
ENTRYPOINT ["/usr/local/bin/dln-node"]
