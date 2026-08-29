# dln-node E2E Container

This folder contains the E2E-focused container build for `dln-node`.

## Build

From repository root:

```bash
docker build --build-context bip321=../bip321 -f tests/e2e/docker/dln-node/Dockerfile -t dln-node:e2e .
```

## Runtime Contract

The container runs as non-root user `dln-node` and uses:

- Working directory: `/var/lib/dln-node`
- Binary: `/usr/local/bin/dln-node`

The current binary expects `config.toml` in the working directory.

Required at runtime:

1. Mount a writable state/config volume to `/var/lib/dln-node`.
2. Provide `/var/lib/dln-node/config.toml`.
3. Ensure relay and bitcoind endpoints in config are reachable from the container network.

Example run:

```bash
docker run --rm \
  -v "$PWD/tests/e2e/runtime:/var/lib/dln-node" \
  --network host \
  dln-node:e2e
```

## Quick Checks

Check image was built:

```bash
docker image inspect dln-node:e2e >/dev/null
```

Check runtime user is non-root:

```bash
docker run --rm --entrypoint id dln-node:e2e -u
```
