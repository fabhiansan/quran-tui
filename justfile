# Dev tasks for quran-tui. Run `just <task>`; `just` alone builds.

default: build

# Compile the binary.
build:
    cargo build

# Run the TUI. Pass args after `--`, e.g. `just run --audio-dir ../TestAssets`.
run *ARGS:
    cargo run -- {{ARGS}}

# Clippy with warnings denied.
lint:
    cargo clippy --all-targets -- -D warnings

# Format in place.
fmt:
    cargo fmt

# Format check (CI).
fmt-check:
    cargo fmt --check

# Run the test suite.
test:
    cargo test

# Full gate: format, lint, test, build.
check: fmt-check lint test build
