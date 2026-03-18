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
test:
    cargo test -- --test-threads=1

# Run the server
run *args:
    cargo run -- {{args}}

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
    cargo run -- config.toml

# Run mdk-server against staging (mutinynet + staging.moneydevkit.com)
dev-staging: dev-staging-config
    cargo run -- config.toml

# Generate config.toml for staging (esplora, no local bitcoind needed)
dev-staging-config:
    #!/usr/bin/env bash
    set -euo pipefail
    set -a; source .env.staging; set +a
    : "${MDK_ACCESS_TOKEN:?set MDK_ACCESS_TOKEN in .env.staging}"
    LSP_NODE_ID="${LSP_NODE_ID:-03fd9a377576df94cc7e458471c43c400630655083dee89df66c6ad38d1b7acffd}"
    LSP_ADDRESS="${LSP_ADDRESS:-lsp.staging.moneydevkit.com:9735}"
    ESPLORA_URL="${ESPLORA_URL:-https://mutinynet.com/api}"
    MDK_API_BASE_URL="${MDK_API_BASE_URL:-https://staging.moneydevkit.com/rpc}"
    cat > config.toml << TOML
    [node]
    network = "signet"
    listening_addresses = ["127.0.0.1:19735"]
    rest_service_address = "127.0.0.1:3099"

    [storage.disk]
    dir_path = "{{dev_storage}}"

    [log]
    level = "Debug"

    [esplora]
    server_url = "$ESPLORA_URL"

    [mdk]
    api_address = "127.0.0.1:8081"
    lsp_node_id = "$LSP_NODE_ID"
    lsp_address = "$LSP_ADDRESS"
    mdk_access_token = "$MDK_ACCESS_TOKEN"
    mdk_api_base_url = "$MDK_API_BASE_URL"
    TOML
    echo "config.toml written (staging/mutinynet)"
    echo "  esplora     $ESPLORA_URL"
    echo "  lsp         $LSP_NODE_ID"
    echo "  mdk         $MDK_API_BASE_URL"

# Wipe mdk-server local state (seed, db, api key)
dev-clean:
    rm -rf "{{dev_storage}}"
    rm -f config.toml
    @echo "Cleaned {{dev_storage}} and config.toml"

[private]
api-key:
    @xxd -p -c 64 "{{dev_storage}}/$(grep -m1 'network' config.toml | sed 's/.*= *\"//;s/\"//')/api_key"

# Print the hex API key for the running mdk-server
dev-api-key:
    @just api-key

# Create a test invoice (amount in msats, default 100k = 100 sats)
dev-invoice amount_msat="100000":
    #!/usr/bin/env bash
    set -euo pipefail
    api_key=$(just api-key)
    resp=$(curl -sS -w '\n%{http_code}' http://127.0.0.1:8081/v1/invoices \
      -H "Authorization: Bearer $api_key" \
      -H "Content-Type: application/json" \
      -d "{\"amountMsat\": {{amount_msat}}, \"description\": \"test\", \"expirySecs\": 3600}")
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
    api_key=$(just api-key)
    curl -s "http://127.0.0.1:8081/v1/invoices/{{payment_hash}}" \
      -H "Authorization: Bearer $api_key" \
    | jq .

# Show node info for the running mdk-server
dev-node-info:
    #!/usr/bin/env bash
    set -euo pipefail
    api_key=$(just api-key)
    curl -s http://127.0.0.1:8081/v1/node \
      -H "Authorization: Bearer $api_key" \
    | jq .

# End-to-end test. Pass --mdk to auto-provision a moneydevkit.com account.
e2e *flags: dev-clean dev-config
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
    # MDK account provisioning (when --mdk flag is passed)
    # ---------------------------------------------------------------
    mdk_enabled=""
    mdk_token=""
    checkout_ids=()

    for flag in {{flags}}; do
      case "$flag" in
        --mdk) mdk_enabled=1 ;;
        *)     echo "Unknown flag: $flag"; exit 1 ;;
      esac
    done

    if [ -n "${MDK_ACCESS_TOKEN:-}" ]; then
      mdk_enabled=1
      mdk_token="$MDK_ACCESS_TOKEN"
    fi

    if [ -n "$mdk_enabled" ] && [ -z "$mdk_token" ]; then
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

      # Inject token into config.toml
      export MDK_ACCESS_TOKEN="$mdk_token"
      export MDK_API_BASE_URL="$mdk_rpc"
      echo "mdk_access_token = \"$mdk_token\"" >> config.toml
      echo "mdk_api_base_url = \"$mdk_rpc\"" >> config.toml
      echo "  mdk         enabled ($mdk_rpc)"
    fi

    # ---------------------------------------------------------------
    # Helpers
    # ---------------------------------------------------------------
    pay_invoice() {
      local label="$1"
      local amount="$2"
      echo ""
      echo "==> [$label] Creating invoice for $amount msat..."
      resp=$(curl -sS http://127.0.0.1:8081/v1/invoices \
        -H "Authorization: Bearer $api_key" \
        -H "Content-Type: application/json" \
        -d "{\"amountMsat\": $amount, \"description\": \"$label\", \"expirySecs\": 3600}")
      echo "$resp" | jq .
      invoice=$(echo "$resp" | jq -r '.invoice')
      payment_hash=$(echo "$resp" | jq -r '.paymentHash')
      checkout_id=$(echo "$resp" | jq -r '.checkoutId // empty')

      if [ -n "$mdk_enabled" ]; then
        if [ -z "$checkout_id" ]; then
          echo "FAIL: MDK integration active but no checkoutId in response"
          return 1
        fi
        echo "==> [$label] checkoutId: $checkout_id"
        checkout_ids+=("$checkout_id")
      fi

      echo "==> [$label] Paying from node2..."
      grpcurl -plaintext -import-path "{{ln_proto}}" -proto lightning.proto \
        -d "{\"bolt11\": \"$invoice\"}" \
        "localhost:$n2_grpc" lightning.LightningNode/SendBolt11Payment

      echo -n "==> [$label] Waiting for settlement"
      for i in {1..30}; do
        status=$(curl -sS "http://127.0.0.1:8081/v1/invoices/$payment_hash" \
          -H "Authorization: Bearer $api_key" | jq -r '.status')
        if [ "$status" = "received" ]; then
          echo " done!"
          curl -sS "http://127.0.0.1:8081/v1/invoices/$payment_hash" \
            -H "Authorization: Bearer $api_key" | jq .
          return 0
        fi
        echo -n "."
        sleep 0.5
      done
      echo " timeout!"
      curl -sS "http://127.0.0.1:8081/v1/invoices/$payment_hash" \
        -H "Authorization: Bearer $api_key" | jq .
      return 1
    }

    # ---------------------------------------------------------------
    # Run
    # ---------------------------------------------------------------
    echo "==> Starting mdk-server..."
    cargo run --quiet -- config.toml &
    SERVER_PID=$!

    echo -n "==> Waiting for API to be ready"
    for i in {1..30}; do
      if curl -sS -o /dev/null http://127.0.0.1:8081/v1/node 2>/dev/null; then
        echo " ready!"
        break
      fi
      echo -n "."
      sleep 0.5
    done

    api_key=$(xxd -p -c 64 "{{dev_storage}}/$(grep -m1 'network' config.toml | sed 's/.*= *\"//;s/\"//')/api_key")
    n2_grpc=$(just compose-port lightning-node2 4000)

    pay_invoice "JIT channel" $amount_msat
    pay_invoice "Existing channel" $amount_msat

    echo ""
    echo "==> Channel info:"
    curl -sS http://127.0.0.1:8081/v1/node \
      -H "Authorization: Bearer $api_key" | jq '.channels'

    # ---------------------------------------------------------------
    # Verify checkouts on moneydevkit.com
    # ---------------------------------------------------------------
    if [ -n "$mdk_enabled" ]; then
      echo ""
      echo "==> Verifying checkout status on moneydevkit.com..."
      # Give the async payment notification a moment to propagate
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
        echo "==> MDK integration: PASS"
      else
        echo ""
        echo "==> MDK integration: FAIL (not all checkouts marked as paid)"
        exit 1
      fi

      if [ -n "${mdk_email:-}" ]; then
        echo ""
        echo "==> Dashboard login:"
        echo "  url       $mdk_url"
        echo "  email     $mdk_email"
        echo "  password  $mdk_password"
      fi
    fi

# Generate config.toml for the running lightning-node stack
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
    cat > config.toml << 'TOML'
    [node]
    network = "regtest"
    listening_addresses = ["127.0.0.1:19735"]
    rest_service_address = "127.0.0.1:3099"

    [storage.disk]
    dir_path = "DEV_STORAGE_PLACEHOLDER"

    [log]
    level = "Debug"

    [bitcoind]
    rpc_address = "BTC_PLACEHOLDER"
    rpc_user = "bitcoind"
    rpc_password = "bitcoind"

    [mdk]
    api_address = "127.0.0.1:8081"
    lsp_node_id = "PUBKEY_PLACEHOLDER"
    lsp_address = "P2P_PLACEHOLDER"
    TOML
    sed -i "s|DEV_STORAGE_PLACEHOLDER|{{dev_storage}}|" config.toml
    sed -i "s|BTC_PLACEHOLDER|127.0.0.1:${btc_port}|" config.toml
    sed -i "s|PUBKEY_PLACEHOLDER|${n1_pubkey}|" config.toml
    sed -i "s|P2P_PLACEHOLDER|127.0.0.1:${n1_p2p}|" config.toml
    if [ -n "${MDK_ACCESS_TOKEN:-}" ]; then
      echo "mdk_access_token = \"${MDK_ACCESS_TOKEN}\"" >> config.toml
      if [ -n "${MDK_API_BASE_URL:-}" ]; then
        echo "mdk_api_base_url = \"${MDK_API_BASE_URL}\"" >> config.toml
      fi
    fi
    echo "config.toml written"
    echo "  bitcoind    127.0.0.1:${btc_port}"
    echo "  node1 p2p   127.0.0.1:${n1_p2p}"
    echo "  node1 id    ${n1_pubkey}"
    if [ -n "${MDK_ACCESS_TOKEN:-}" ]; then
      echo "  mdk         enabled (${MDK_API_BASE_URL:-https://staging.moneydevkit.com/rpc})"
    fi