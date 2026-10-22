# ArkForge developer tasks.
#
# Recipes wrap the commands already used by AGENTS.md and
# .github/workflows/rust.yml. `just` is a local convenience. It is not a
# runtime dependency, and the workspace Cargo manifests do not depend on it.
#
# `ci` matches the macOS Rust workflow. Windows packaging guards stay in
# .github/workflows/windows.yml and are not part of this recipe.

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# List recipes.
default:
	@just --list

# Build the workspace.
build:
	cargo build --workspace

# Run every workspace test target.
test:
	cargo test --workspace --all-targets

# Lint with the CI clippy gate.
lint:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

# Format Rust sources in place.
fmt:
	cargo fmt --all

# Host checks from .github/workflows/rust.yml.
ci:
	cargo fmt --all -- --check
	sh scripts/test-preboot-holder.sh
	python3 scripts/test-preboot-init.py
	python3 scripts/test-preboot-report.py
	python3 -m unittest discover -s scripts -p test_restore_boot_loop.py
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cargo test --workspace --all-targets
	swift test
