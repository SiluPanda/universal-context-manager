.PHONY: check fmt lint test test-rust test-desktop test-e2e build build-rust build-desktop bundle-desktop prepare-sidecars install-local validate-plugins clean

check: fmt lint test build

fmt:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd apps/desktop && pnpm lint

test: test-rust test-desktop validate-plugins

test-rust:
	cargo test --workspace --all-features

test-desktop:
	cd apps/desktop && pnpm test

test-e2e:
	./scripts/e2e-smoke.sh

build: build-rust build-desktop

build-rust:
	cargo build --workspace --all-features

build-desktop:
	cd apps/desktop && pnpm build

prepare-sidecars:
	./scripts/prepare-sidecars.sh release

install-local:
	./scripts/install-local.sh

bundle-desktop:
	cd apps/desktop && pnpm tauri build

validate-plugins:
	./scripts/validate-adapters.sh

clean:
	cargo clean
	rm -rf apps/desktop/dist apps/desktop/node_modules
