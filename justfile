default:
    @just --list

# Run all checks (fmt, clippy, test)
check: fmt-check clippy

# Format code
fmt:
    cargo fmt --all
    nixfmt flake.nix

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check
    nixfmt --check flake.nix

# Auto-fix lint issues
fix:
    cargo clippy --fix --allow-dirty --allow-staged

# Run clippy check
clippy:
    cargo clippy -- -D warnings

# Run tests
test *args:
    cargo nextest run {{args}}

# Run the server
run *args:
    cargo run -- {{args}}

# Build the static musl binary
build-static:
    nix build .#static

# Run the static binary
run-static *args:
    nix run .#static -- {{args}}

# Build and load the docker image
build-image:
    nix build .#image && docker load < result

# Clean build artifacts
clean:
    cargo clean

# ---------------------------------------------------------------------------
# Local dev against the lightning-node docker-compose stack
# ---------------------------------------------------------------------------

ln_dir      := env("LN_DIR", env("HOME") / "Development/mdk/lightning-node")
dev_storage := justfile_directory() / ".data"
ln_proto    := ln_dir / "lightning-node/proto"
rm_proto    := ln_dir / "regminer/proto"

[private]
grpc-ln port method data="{}":
    @grpcurl -plaintext -import-path "{{ln_proto}}" -proto lightning.proto -d '{{data}}' "localhost:{{port}}" "lightning.LightningNode/{{method}}"

[private]
grpc-rm method data="{}":
    #!/usr/bin/env bash
    set -euo pipefail
    port=$(docker compose -f "{{ln_dir}}/docker-compose.yml" port regminer 3700 2>/dev/null | cut -d: -f2)
    grpcurl -plaintext -import-path "{{rm_proto}}" -proto regminer.proto -d '{{data}}' "localhost:$port" "regminer.RegMiner/{{method}}"

[private]
compose-port service port:
    @docker compose -f "{{ln_dir}}/docker-compose.yml" port {{service}} {{port}} 2>/dev/null | cut -d: -f2

# Run mdk-server against the local lightning-node stack
dev: dev-config
    #!/usr/bin/env bash
    set -euo pipefail
    set -a; source .env; set +a
    : "${MDK_ACCESS_TOKEN:?set MDK_ACCESS_TOKEN in .env}"
    : "${MDK_HTTP_PASSWORD_FULL:?set MDK_HTTP_PASSWORD_FULL in .env}"
    : "${MDK_HTTP_PASSWORD_READ_ONLY:?set MDK_HTTP_PASSWORD_READ_ONLY in .env}"
    export MDK_MNEMONIC="abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
    cargo run -- config.toml

# Run mdk-server against staging (mutinynet + staging.moneydevkit.com)
dev-staging: dev-staging-config
    #!/usr/bin/env bash
    set -euo pipefail
    set -a; source .env; set +a
    : "${MDK_ACCESS_TOKEN:?set MDK_ACCESS_TOKEN in .env}"
    : "${MDK_HTTP_PASSWORD_FULL:?set MDK_HTTP_PASSWORD_FULL in .env}"
    : "${MDK_HTTP_PASSWORD_READ_ONLY:?set MDK_HTTP_PASSWORD_READ_ONLY in .env}"
    : "${MDK_WEBHOOK_SECRET:?set MDK_WEBHOOK_SECRET in .env}"
    : "${MDK_MNEMONIC:?set MDK_MNEMONIC in .env}"
    export MDK_API_BASE_URL="${MDK_API_BASE_URL:-https://staging.moneydevkit.com/rpc}"
    cargo run -- config.toml

# Generate config.toml + .env for staging (esplora, no local bitcoind needed)
dev-staging-config:
    #!/usr/bin/env bash
    set -euo pipefail
    ESPLORA_URL="${ESPLORA_URL:-https://mutinynet.com/api}"
    cat > config.toml << TOML
    [node]
    network = "signet"
    listening_addresses = ["127.0.0.1:19735"]
    rest_service_address = "127.0.0.1:8081"

    [storage.disk]
    dir_path = "{{dev_storage}}"

    [log]
    level = "Debug"

    [esplora]
    server_url = "$ESPLORA_URL"
    TOML
    echo "config.toml written (staging/signet)"
    echo "  esplora     $ESPLORA_URL"
    # Write .env only if it doesn't already exist (user manages staging credentials)
    if [ ! -f .env ]; then
      cat > .env << 'ENV'
    # Fill these in for staging:
    # MDK_ACCESS_TOKEN=
    # MDK_MNEMONIC=
    MDK_API_BASE_URL=https://staging.moneydevkit.com/rpc
    MDK_WEBHOOK_SECRET=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    MDK_HTTP_PASSWORD_FULL=staging_full_password
    MDK_HTTP_PASSWORD_READ_ONLY=staging_readonly_password
    ENV
      echo ".env template written (fill in MDK_ACCESS_TOKEN and MDK_MNEMONIC)"
    fi

# Wipe mdk-server local state (seed, db, api key)
dev-clean:
    rm -rf "{{dev_storage}}"
    rm -f config.toml
    @echo "Cleaned {{dev_storage}} and config.toml"

[private]
http-password:
    #!/usr/bin/env bash
    set -euo pipefail
    set -a; source .env; set +a
    echo "$MDK_HTTP_PASSWORD_FULL"

# Print the full-access password for the running mdk-server
dev-password:
    @just http-password

# Create a test invoice (amount in sats, default 100)
dev-invoice amount_sat="100":
    #!/usr/bin/env bash
    set -euo pipefail
    pw=$(just http-password)
    resp=$(curl -sS -w '\n%{http_code}' http://127.0.0.1:8081/createinvoice \
      -u ":$pw" \
      -d "amountSat={{amount_sat}}&description=test&expirySeconds=3600")
    code=$(echo "$resp" | tail -1)
    body=$(echo "$resp" | sed '$d')
    if [ "$code" -ge 400 ] 2>/dev/null || [ -z "$body" ]; then
      echo "HTTP $code"
      echo "$body"
      exit 1
    fi
    echo "$body" | jq .

# Pay a bolt11 invoice from node2 in the lightning-node stack
dev-pay invoice:
    #!/usr/bin/env bash
    set -euo pipefail
    n2_grpc=$(just compose-port lightning-node2 4000)
    just grpc-ln "$n2_grpc" SendBolt11Payment '{"bolt11": "{{invoice}}"}'

# Check invoice status by payment_hash
dev-status payment_hash:
    #!/usr/bin/env bash
    set -euo pipefail
    pw=$(just http-password)
    curl -s "http://127.0.0.1:8081/v1/invoices/{{payment_hash}}" \
      -u ":$pw" \
    | jq .

# Show node info for the running mdk-server
dev-get-info:
    #!/usr/bin/env bash
    set -euo pipefail
    pw=$(just http-password)
    curl -s http://127.0.0.1:8081/getinfo \
      -u ":$pw" \
    | jq .

# End-to-end test against the local lightning-node + moneydevkit stack
e2e: dev-clean
    #!/usr/bin/env bash
    set -euo pipefail
    amount_msat=100000
    mdk_url="http://localhost:3900"
    mdk_rpc="$mdk_url/rpc"

    cleanup() {
      if [ -n "${SERVER_PID:-}" ]; then
        echo "==> Stopping mdk-server (pid $SERVER_PID)..."
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
      fi
    }
    trap cleanup EXIT

    # ---------------------------------------------------------------
    # MDK account provisioning
    # ---------------------------------------------------------------
    checkout_ids=()

    echo "==> Provisioning moneydevkit.com account..."
    mdk_email="mdk-e2e-${RANDOM}@test.local"
    mdk_password="E2eTestPass99"
    signup=$(curl -sS "$mdk_url/api/auth/sign-up/email" \
      -H 'Content-Type: application/json' \
      -d "{\"email\":\"$mdk_email\",\"password\":\"$mdk_password\",\"name\":\"mdk-server e2e\"}")
    session=$(echo "$signup" | jq -r '.token // empty')
    if [ -z "$session" ]; then
      echo "FAIL: signup failed"
      echo "$signup" | jq .
      exit 1
    fi
    echo "  account     $mdk_email"

    app=$(curl -sS "$mdk_url/api/mcp/apps" \
      -H 'Content-Type: application/json' \
      -H "Authorization: Bearer $session" \
      -d '{"name":"mdk-server-e2e","webhookUrl":"http://localhost:8081/webhook"}')
    mdk_token=$(echo "$app" | jq -r '.apiKey // empty')
    if [ -z "$mdk_token" ]; then
      echo "FAIL: app creation failed"
      echo "$app" | jq .
      exit 1
    fi
    echo "  api key     ${mdk_token:0:15}..."

    export MDK_ACCESS_TOKEN="$mdk_token"
    export MDK_API_BASE_URL="$mdk_rpc"
    export MDK_WEBHOOK_SECRET="${MDK_WEBHOOK_SECRET:-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa}"
    export MDK_HTTP_PASSWORD_FULL="${MDK_HTTP_PASSWORD_FULL:-e2e_full_password}"
    export MDK_HTTP_PASSWORD_READ_ONLY="${MDK_HTTP_PASSWORD_READ_ONLY:-e2e_readonly_password}"
    just dev-config
    # Source .env written by dev-config (picks up MDK_LSP_NODE_ID etc.)
    set -a; source .env; set +a
    http_pw="$MDK_HTTP_PASSWORD_FULL"

    # ---------------------------------------------------------------
    # Helpers
    # ---------------------------------------------------------------
    pay_invoice() {
      local label="$1"
      local amount="$2"
      echo ""
      local amount_sat=$((amount / 1000))
      echo "==> [$label] Creating invoice for $amount_sat sat..."
      resp=$(curl -sS http://127.0.0.1:8081/createinvoice \
        -u ":$http_pw" \
        -d "amountSat=$amount_sat&description=$label&expirySeconds=3600")
      echo "$resp" | jq .
      invoice=$(echo "$resp" | jq -r '.serialized')
      payment_hash=$(echo "$resp" | jq -r '.paymentHash')

      echo "==> [$label] Paying from node2..."
      grpcurl -plaintext -import-path "{{ln_proto}}" -proto lightning.proto \
        -d "{\"bolt11\": \"$invoice\"}" \
        "localhost:$n2_grpc" lightning.LightningNode/SendBolt11Payment

      echo -n "==> [$label] Waiting for settlement"
      for i in {1..30}; do
        status=$(curl -sS "http://127.0.0.1:8081/v1/invoices/$payment_hash" \
          -u ":$http_pw" | jq -r '.status')
        if [ "$status" = "received" ]; then
          echo " done!"
          curl -sS "http://127.0.0.1:8081/v1/invoices/$payment_hash" \
            -u ":$http_pw" | jq .
          return 0
        fi
        echo -n "."
        sleep 0.5
      done
      echo " timeout!"
      curl -sS "http://127.0.0.1:8081/v1/invoices/$payment_hash" \
        -u ":$http_pw" | jq .
      return 1
    }

    # ---------------------------------------------------------------
    # Run
    # ---------------------------------------------------------------
    echo "==> Starting mdk-server..."
    export MDK_MNEMONIC="${MDK_MNEMONIC:-abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about}"
    cargo run --quiet -- config.toml &
    SERVER_PID=$!

    echo -n "==> Waiting for API to be ready"
    for i in {1..30}; do
      if curl -sS -o /dev/null http://127.0.0.1:8081/getinfo 2>/dev/null; then
        echo " ready!"
        break
      fi
      echo -n "."
      sleep 0.5
    done

    n2_grpc=$(just compose-port lightning-node2 4000)

    pay_invoice "JIT channel" $amount_msat
    pay_invoice "Existing channel" $amount_msat

    echo ""
    echo "==> Channel info:"
    curl -sS http://127.0.0.1:8081/getinfo \
      -u ":$http_pw" | jq '.channels'

    # ---------------------------------------------------------------
    # Verify checkouts on moneydevkit.com
    # ---------------------------------------------------------------
    echo ""
    echo "==> Verifying checkout status on moneydevkit.com..."
    sleep 1
    all_paid=true
    for cid in "${checkout_ids[@]}"; do
      status=$(curl -sS "$mdk_rpc/checkout/get" \
        -H 'Content-Type: application/json' \
        -H "x-api-key: $mdk_token" \
        -d "{\"json\":{\"id\":\"$cid\"}}" | jq -r '.json.status')
      if [ "$status" = "PAYMENT_RECEIVED" ]; then
        echo "  $cid  $status"
      else
        echo "  $cid  $status  (expected PAYMENT_RECEIVED)"
        all_paid=false
      fi
    done

    if [ "$all_paid" = true ]; then
      echo ""
      echo "==> All checkouts paid: PASS"
    else
      echo ""
      echo "==> FAIL: not all checkouts marked as paid"
      exit 1
    fi

    echo ""
    echo "==> Dashboard login:"
    echo "  url       $mdk_url"
    echo "  email     $mdk_email"
    echo "  password  $mdk_password"

# Generate config.toml + .env for the running lightning-node stack
dev-config:
    #!/usr/bin/env bash
    set -euo pipefail
    LN="{{ln_dir}}"
    COMPOSE="docker compose -f $LN/docker-compose.yml"
    btc_port=$($COMPOSE port bitcoind 18443 2>/dev/null | cut -d: -f2) || true
    n1_p2p=$($COMPOSE port lightning-node1 9735 2>/dev/null | cut -d: -f2) || true
    n1_grpc=$($COMPOSE port lightning-node1 4000 2>/dev/null | cut -d: -f2) || true
    for var in btc_port n1_p2p n1_grpc; do
      if [ -z "${!var}" ]; then
        echo "ERROR: lightning-node stack not running (missing port for $var)."
        echo "       Start it first:  cd $LN && just dev --clean"
        exit 1
      fi
    done
    n1_pubkey=$(grpcurl -plaintext \
      -import-path "$LN/lightning-node/proto" -proto lightning.proto \
      "localhost:$n1_grpc" lightning.LightningNode/GetNodeInfo \
      | jq -r '.nodeId')
    if [ -z "$n1_pubkey" ] || [ "$n1_pubkey" = "null" ]; then
      echo "ERROR: could not fetch node1 pubkey via gRPC on port $n1_grpc"
      exit 1
    fi
    cat > config.toml << TOML
    [node]
    network = "regtest"
    listening_addresses = ["127.0.0.1:19735"]
    rest_service_address = "127.0.0.1:8081"

    [storage.disk]
    dir_path = "{{dev_storage}}"

    [log]
    level = "Debug"

    [bitcoind]
    rpc_address = "127.0.0.1:${btc_port}"
    rpc_user = "bitcoind"
    rpc_password = "bitcoind"
    TOML
    cat > .env << ENV
    MDK_LSP_NODE_ID=${n1_pubkey}
    MDK_LSP_ADDRESS=127.0.0.1:${n1_p2p}
    MDK_API_BASE_URL=${MDK_API_BASE_URL:-http://localhost:3900/rpc}
    MDK_WEBHOOK_SECRET=${MDK_WEBHOOK_SECRET:-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa}
    MDK_HTTP_PASSWORD_FULL=${MDK_HTTP_PASSWORD_FULL:-dev_full_password}
    MDK_HTTP_PASSWORD_READ_ONLY=${MDK_HTTP_PASSWORD_READ_ONLY:-dev_readonly_password}
    ENV
    echo "config.toml written"
    echo "  bitcoind    127.0.0.1:${btc_port}"
    echo "  node1 p2p   127.0.0.1:${n1_p2p}"
    echo "  node1 id    ${n1_pubkey}"
    echo "  mdk api     ${MDK_API_BASE_URL:-http://localhost:3900/rpc}"