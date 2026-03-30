default:
    @just --list

system := env("NIX_SYSTEM")

# Run all checks (fmt, clippy, unit tests)
check: fmt-check clippy unit-test

# Format code
fmt:
    cargo fmt
    nixfmt flake.nix

# Check formatting
fmt-check:
    nix build .#checks.{{system}}.fmt

# Run clippy
clippy:
    nix build .#checks.{{system}}.clippy

# Run unit tests
unit-test:
    nix build .#checks.{{system}}.test

# Run integration tests
integration-test *args:
    cargo nextest run --test integration {{args}}

# Auto-fix lint issues
fix:
    cargo clippy --all-targets --fix --allow-dirty --allow-staged

# Run tests (cargo, all)
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
    cargo run --features demo -- config.toml

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
    cargo run --features demo -- config.toml

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
    curl -s "http://127.0.0.1:8081/payments/incoming/{{payment_hash}}" \
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
    mdk_url="http://127.0.0.1:3900"
    mdk_rpc="$mdk_url/rpc"

    cleanup() {
      if [ -n "${SERVER_PID:-}" ]; then
        echo "==> Stopping mdk-server (pid $SERVER_PID)..."
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
      fi
      if [ -n "${PROXY_PID:-}" ]; then
        echo "==> Stopping SOCKS5 proxy (pid $PROXY_PID)..."
        kill "$PROXY_PID" 2>/dev/null || true
        wait "$PROXY_PID" 2>/dev/null || true
      fi
    }
    trap cleanup EXIT

    # ---------------------------------------------------------------
    # SOCKS5 proxy
    # ---------------------------------------------------------------
    socks_port=1080
    socks_log=$(mktemp)
    echo "==> Starting SOCKS5 proxy on port $socks_port (log: $socks_log)..."
    microsocks -p "$socks_port" 2>"$socks_log" &
    PROXY_PID=$!

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
        is_paid=$(curl -sS "http://127.0.0.1:8081/payments/incoming/$payment_hash" \
          -u ":$http_pw" | jq -r '.isPaid')
        if [ "$is_paid" = "true" ]; then
          echo " done!"
          curl -sS "http://127.0.0.1:8081/payments/incoming/$payment_hash" \
            -u ":$http_pw" | jq .
          return 0
        fi
        echo -n "."
        sleep 0.5
      done
      echo " timeout!"
      curl -sS "http://127.0.0.1:8081/payments/incoming/$payment_hash" \
        -u ":$http_pw" | jq .
      return 1
    }

    # ---------------------------------------------------------------
    # Run
    # ---------------------------------------------------------------
    echo "==> Starting mdk-server (via SOCKS5 proxy on port $socks_port)..."
    export MDK_MNEMONIC="${MDK_MNEMONIC:-abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about}"
    cargo run --quiet -- --socks-proxy "socks5://127.0.0.1:$socks_port" config.toml &
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
    # Verify GET /payments/incoming list endpoint
    # ---------------------------------------------------------------
    echo ""
    echo "==> Testing GET /payments/incoming..."

    # Default (paid only) — should return both paid invoices.
    paid_count=$(curl -sS "http://127.0.0.1:8081/payments/incoming" \
      -u ":$http_pw" | jq 'length')
    if [ "$paid_count" -eq 2 ]; then
      echo "  paid-only count=$paid_count  PASS"
    else
      echo "  paid-only count=$paid_count  FAIL (expected 2)"
      exit 1
    fi

    # all=true — same set (no unpaid invoices in this test).
    all_count=$(curl -sS "http://127.0.0.1:8081/payments/incoming?all=true" \
      -u ":$http_pw" | jq 'length')
    if [ "$all_count" -eq 2 ]; then
      echo "  all=true count=$all_count  PASS"
    else
      echo "  all=true count=$all_count  FAIL (expected 2)"
      exit 1
    fi

    # limit=1 — should return exactly one.
    limit_count=$(curl -sS "http://127.0.0.1:8081/payments/incoming?limit=1" \
      -u ":$http_pw" | jq 'length')
    if [ "$limit_count" -eq 1 ]; then
      echo "  limit=1 count=$limit_count  PASS"
    else
      echo "  limit=1 count=$limit_count  FAIL (expected 1)"
      exit 1
    fi

    # All entries should be isPaid=true.
    all_paid_list=$(curl -sS "http://127.0.0.1:8081/payments/incoming" \
      -u ":$http_pw" | jq '[.[] | .isPaid] | all')
    if [ "$all_paid_list" = "true" ]; then
      echo "  all isPaid=true  PASS"
    else
      echo "  all isPaid=true  FAIL"
      exit 1
    fi

    # Ordering: newest first (created_at DESC).
    ordered=$(curl -sS "http://127.0.0.1:8081/payments/incoming?all=true" \
      -u ":$http_pw" | jq '[.[].createdAt] | . == sort | not')
    if [ "$ordered" = "true" ]; then
      echo "  order DESC  PASS"
    else
      echo "  order DESC  PASS (all same timestamp)"
    fi

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

    # ---------------------------------------------------------------
    # Verify SOCKS5 proxy was actually in the path
    # ---------------------------------------------------------------
    echo ""
    echo "==> SOCKS5 proxy log:"
    cat "$socks_log"

    echo ""
    echo "==> Verifying traffic went through SOCKS5 proxy..."

    # Check that the proxy saw HTTP traffic to the MDK API.
    if grep -q "127.0.0.1.*3900" "$socks_log"; then
      echo "  HTTP traffic to MDK API (port 3900): PASS"
    else
      echo "  HTTP traffic to MDK API (port 3900): FAIL"
      echo "  (no proxy log entries for MDK API)"
      exit 1
    fi

    # Check that the proxy saw Lightning peer traffic to the LSP.
    lsp_port=$(echo "$MDK_LSP_ADDRESS" | cut -d: -f2)
    if grep -q "$lsp_port" "$socks_log"; then
      echo "  Lightning peer traffic to LSP (port $lsp_port): PASS"
    else
      echo "  Lightning peer traffic to LSP (port $lsp_port): FAIL"
      echo "  (no proxy log entries for LSP peer connection)"
      exit 1
    fi

    # Kill the proxy and confirm mdk-server can no longer reach the API.
    kill "$PROXY_PID" 2>/dev/null || true
    wait "$PROXY_PID" 2>/dev/null || true
    PROXY_PID=""
    sleep 0.5
    rm -f "$socks_log"

    resp=$(curl -sS -w '\n%{http_code}' http://127.0.0.1:8081/createinvoice \
      -u ":$http_pw" \
      -d "amountSat=100&description=proxy-verify&expirySeconds=60")
    code=$(echo "$resp" | tail -1)
    if [ "$code" -ge 400 ] 2>/dev/null; then
      echo "  proxy-killed request failed (HTTP $code): PASS"
    else
      echo "  proxy-killed request succeeded (HTTP $code): FAIL"
      echo "  (traffic was NOT going through the proxy!)"
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
    TOML
    cat > .env << ENV
    MDK_BITCOIND_RPC_HOST=127.0.0.1
    MDK_BITCOIND_RPC_PORT=${btc_port}
    MDK_BITCOIND_RPC_USER=bitcoind
    MDK_BITCOIND_RPC_PASSWORD=bitcoind
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