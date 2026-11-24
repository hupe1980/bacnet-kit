# List the available recipes.
default:
    @just --list

# Build the default workstation CLI.
build:
    cargo build --locked

# Read a simulated temperature on loopback.
demo:
    cargo run --locked -- demo

# Format the library, CLI, and firmware fixture.
fmt:
    cargo fmt --all
    cd checks/cortex-m4 && cargo fmt

# Check formatting without changing files.
fmt-check:
    cargo fmt --all --check
    cd checks/cortex-m4 && cargo fmt --check

# Run Clippy across all host targets and features.
lint:
    cargo clippy --locked --all-targets --all-features -- -D warnings

# Test the default, all-feature, std-only, and core-only configurations.
test:
    cargo test --locked
    cargo test --locked --all-features
    cargo test --locked --no-default-features
    cargo test --locked --no-default-features --features std

# Build the Rust API reference with warnings treated as errors.
docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --all-features --no-deps

# Run the host formatting, lint, test, and API documentation checks.
check: fmt-check lint test docs

# Check the allocator-free library for thumbv7em-none-eabihf.
embedded-check:
    cargo check --locked --no-default-features --target thumbv7em-none-eabihf

# Run the Cortex-M4 fixture with qemu-system-arm.
firmware:
    cd checks/cortex-m4 && cargo run --locked --release

# Preview the Zola landing page and field guide with live reload.
site:
    cd site && zola serve

# Check internal links and build the static site.
site-build:
    cd site && zola check --skip-external-links && zola build
