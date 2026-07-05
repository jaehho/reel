# reel — Tauri footage manager. See README.md.
# The frontend is static and embedded at compile time, so `cargo run` is all you
# need to launch the app; npm/tauri-cli only come in for release bundling.

.PHONY: run test build fmt clean dump

run: ## launch the app (debug build)
	cargo run -p reel-tauri

test: ## engine tests — headless, fast, no GUI deps
	cargo test -p reel-core

dump: ## print what the engine sees (trips + inserted card) as JSON
	cargo run -p reel-core --example dump

build: ## optimized release binary -> target/release/reel
	cargo build -p reel-tauri --release

fmt:
	cargo fmt

clean:
	cargo clean
