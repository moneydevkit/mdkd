# mdkd

Lightning payment server built on LDK. Connects to the MoneyDevKit platform
for checkout management and uses LSPS4 for JIT channel liquidity.

## Building from source

### With Nix (recommended)

[Nix](https://determinate.systems/nix/) handles the entire toolchain -- Rust, protobuf,
musl cross-compiler, everything. No system dependencies to install manually.

```bash
# Enter the dev shell (or use direnv)
nix develop

# Debug build
cargo build

# Run tests
just check

# Static musl binary (Linux only)
nix build .#static
# -> result/bin/mdkd
```

### Without Nix

You need:

- **Rust** stable (edition 2021)
- **protobuf** compiler (`protoc`)

```bash
# Ubuntu / Debian
sudo apt install protobuf-compiler

# macOS
brew install protobuf

# Arch
sudo pacman -S protobuf
```

Then build with cargo:

```bash
cargo build --release
# -> target/release/mdkd
```

## Running

```bash
# Production -- secrets via file descriptors, nothing in the environment
mdkd config.toml \
  --mnemonic-fd 3 --webhook-secret-fd 4 \
  --password-full-fd 5 --password-read-only-fd 6 \
  --access-token-fd 7 \
  3< <(vault kv get -field=mnemonic secret/mdk) \
  4< <(vault kv get -field=webhook_secret secret/mdk) \
  5< <(vault kv get -field=password_full secret/mdk) \
  6< <(vault kv get -field=password_read_only secret/mdk) \
  7< <(vault kv get -field=access_token secret/mdk)

# Route all traffic through Tor or another SOCKS5 proxy
mdkd config.toml --socks-proxy socks5://127.0.0.1:9050

# Local dev -- env var fallback, no ceremony
source .env && mdkd config.toml
```

## Demo wallet

Build with `--features demo` to serve a single-page wallet UI at `/`:

```bash
cargo run --features demo -- config.toml
```

The wallet connects to the API with basic auth and shows balance,
channels, payment history, and invoice creation. Payment notifications
arrive over WebSocket; a 30s poll acts as fallback.

## API documentation

Interactive API docs are served at `/scalar` (no auth required). This is
auto-generated from the OpenAPI 3.1 spec via [Scalar](https://scalar.com/).

```
http://127.0.0.1:8081/scalar
```

## Configuration

### config.toml

A minimal mainnet example:

```toml
[node]
network = "bitcoin"
listening_addresses = ["0.0.0.0:9735"]
rest_service_address = "127.0.0.1:8081"

[storage.disk]
dir_path = "/var/lib/mdkd"

[log]
level = "Info"
```

`rest_service_address` is the bind address for the server API.

### Secrets

Secrets can be provided via **file descriptors** (preferred) or **environment
variables** (fallback). FD-based passing avoids leaking secrets through
`/proc/<pid>/environ`, `ps e`, and child process inheritance.

For each secret, pass `--<name>-fd N` where `N` is an open file descriptor
containing the value. If the flag is omitted, the corresponding env var is
checked. If neither is present the process exits with an error.

#### Additional CLI flags

| Flag | Description |
|------|-------------|
| `--socks-proxy <url>` | Route all outbound traffic (LDK peer connections and HTTP) through a SOCKS5 proxy. Example: `socks5://127.0.0.1:9050`. |

#### Required on all networks

| CLI flag | Env var fallback | Description |
|----------|------------------|-------------|
| `--access-token-fd N` | `MDK_ACCESS_TOKEN` | API key for the MoneyDevKit platform. Obtain from your [moneydevkit.com](https://moneydevkit.com) dashboard after creating an app. |
| `--mnemonic-fd N` | `MDK_MNEMONIC` | BIP-39 mnemonic phrase for wallet seed derivation. |
| `--password-full-fd N` | `MDK_HTTP_PASSWORD_FULL` | HTTP Basic Auth password granting full access to all API endpoints. You choose this value. |
| `--password-read-only-fd N` | `MDK_HTTP_PASSWORD_READ_ONLY` | HTTP Basic Auth password granting read-only access (node info, invoice lookup). You choose this value. |
| `--webhook-secret-fd N` | `MDK_WEBHOOK_SECRET` | Hex-encoded secret for HMAC-signing outgoing webhook payloads. Must be valid hex, any length. |

#### Regtest only

| Variable | Description |
|----------|-------------|
| `MDK_LSP_NODE_ID` | Public key of the MoneyDevKit lightning node. |
| `MDK_LSP_ADDRESS` | `host:port` of the MoneyDevKit lightning P2P socket. |
| `MDK_API_BASE_URL` | Base URL of the MoneyDevKit RPC API (e.g. `http://localhost:3900/rpc`). |
| `MDK_BITCOIND_RPC_HOST` | Hostname of the bitcoind RPC server. |
| `MDK_BITCOIND_RPC_PORT` | Port of the bitcoind RPC server. |
| `MDK_BITCOIND_RPC_USER` | Bitcoind RPC username. |
| `MDK_BITCOIND_RPC_PASSWORD` | Bitcoind RPC password. |
| `MDK_VSS_URL` | URL of the VSS instance (e.g. `http://localhost:8080/vss`). |

### Generating secrets

**MDK_ACCESS_TOKEN** -- sign in to [moneydevkit.com](https://moneydevkit.com),
create an app, and copy the API key.

**MDK_MNEMONIC** -- Same as above but hit `Generate Mnemonic` instead of copying the key or
generate your own fresh BIP-39 mnemonic. Back it up offline. This is your wallet seed.

**MDK_HTTP_PASSWORD_FULL** / **MDK_HTTP_PASSWORD_READ_ONLY** -- the API uses
HTTP Basic Auth with two password tiers. The username is ignored (convention:
leave it empty). Full access can hit every endpoint; read-only is restricted
to GET routes (node info, invoice lookup). Pick something long and random for
each:

```
openssl rand -hex 32   # MDK_HTTP_PASSWORD_FULL
openssl rand -hex 32   # MDK_HTTP_PASSWORD_READ_ONLY
```

Clients authenticate with `curl -u ":<password>"` or the equivalent
`Authorization: Basic <base64(:password)>` header.

**MDK_WEBHOOK_SECRET** -- hex-encoded bytes. Used as the HMAC-SHA256 key
for signing outgoing webhook payloads. Your webhook receiver needs this
same value to verify signatures.

```
openssl rand -hex 32
```

### Supported networks

| Network | LSP + chain source | VSS |
|---------|-------------------|-----|
| `bitcoin` (mainnet) | Hard-coded | Hard-coded |
| `signet` (mutinynet) | Hard-coded | Hard-coded |
| `regtest` | Env vars | `MDK_VSS_URL` env var |
| `testnet` | Not yet supported | -- |

## Storage

Wallet state (channels, keys, routing scores) is replicated to a remote
[Versioned Storage Service](https://github.com/lightningdevkit/vss-server)
(VSS). This gives you encrypted cloud backup of node state out of the box.

Local data (invoice metadata, outgoing payment tracking) lives in a SQLite
database at `<storage_dir>/<network>/mdkd.sqlite`.

## CI and releases

### CI

Every push to master and every PR runs `nix flake check` (clippy, fmt, unit
tests) and integration tests. On master pushes only, static binaries for
x86_64 and aarch64 are built and uploaded as workflow artifacts.

### Releasing

Bump the version in `Cargo.toml`, tag it, and push:

```
# edit Cargo.toml version field
git commit -am "Bump version to 0.1.0"
git tag v0.1.0
git push --tags
```

The tag must match the version in `Cargo.toml` (e.g. tag `v0.1.0` requires
`version = "0.1.0"`). The release workflow checks this and fails early if
they differ.

The workflow then builds static binaries for both architectures, creates a
GitHub Release with them attached, builds container images via Nix, and pushes
a multi-arch image to GHCR tagged as the version. The `latest` tag is only
updated when the tag is the highest semver.

## Local development

Enter the dev shell with `nix develop` (or use direnv). This gives you the
Rust toolchain, cargo-nextest, protobuf, and the `just` recipes below.

```
just check        # fmt + clippy + unit tests (nix-based)
just fmt          # cargo fmt + nixfmt
just clippy       # cargo clippy via nix
just unit-test    # unit tests via nix
just test         # cargo nextest (all tests)
just build-static # musl static binary
just build-image  # nix docker image, loaded into local docker
just clean        # cargo clean
```

### Running against the LSP stack

Full local development (regtest, e2e tests) requires the
private LSP docker-compose stack.

```
just dev          # regtest against local lightning-node stack
just dev-staging  # signet against staging.moneydevkit.com
just e2e          # full end-to-end test
```

`just dev-config` writes `config.toml` and `.env` from the running
docker-compose stack. `just dev` calls it automatically.
