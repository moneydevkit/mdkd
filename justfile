default:
    @just --list

# Run all checks (fmt, clippy, test)
check: fmt-check clippy test

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
