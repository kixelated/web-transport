#!/usr/bin/env just --justfile

# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation

export RUST_BACKTRACE := "1"
export RUST_LOG := "debug"

# List all of the available commands.
default:
  just --list

# Install any required dependencies.
setup:
	# Install cargo-binstall for faster tool installation.
	cargo install cargo-binstall
	just setup-tools

# A separate entrypoint for CI.
setup-tools:
	cargo binstall -y cargo-shear cargo-sort cargo-upgrades cargo-edit

# Run the CI checks
check:
	cargo check --all-targets --all-features
	cargo clippy --all-targets --all-features -- -D warnings

	# Do the same but explicitly use the WASM target.
	cargo check --all-targets --all-features --target wasm32-unknown-unknown -p web-transport
	cargo clippy --all-targets --all-features --target wasm32-unknown-unknown -p web-transport -- -D warnings

	# Make sure the formatting is correct.
	cargo fmt -- --check

	# requires: cargo install cargo-shear
	cargo shear

	# requires: cargo install cargo-sort
	cargo sort --workspace --check

	# JavaScript/TypeScript checks
	pnpm install --frozen-lockfile
	pnpm -r run check

# Run any CI tests
test:
	cargo test

# Automatically fix some issues.
fix:
	cargo fix --allow-staged --all-targets --all-features
	cargo clippy --fix --allow-staged --all-targets --all-features

	# Do the same but explicitly use the WASM target.
	cargo fix --allow-staged --all-targets --all-features --target wasm32-unknown-unknown -p web-transport
	cargo clippy --fix --allow-staged --all-targets --all-features --target wasm32-unknown-unknown -p web-transport

	# requires: cargo install cargo-shear
	cargo shear --fix

	# requires: cargo install cargo-sort
	cargo sort --workspace

	# And of course, make sure the formatting is correct.
	cargo fmt --all

	# JavaScript/TypeScript fixes
	pnpm install

# Upgrade any tooling
upgrade:
	rustup upgrade

	# Requires: cargo install cargo-upgrades cargo-edit
	cargo upgrade
