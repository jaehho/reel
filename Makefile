# reel — Tauri footage manager. See README.md.
# The frontend is static and embedded at compile time, so `cargo run` is all you
# need to launch the app; npm/tauri-cli only come in for release bundling.

.PHONY: run test build fmt clean dump logs errors

REEL_LOG := $(or $(XDG_STATE_HOME),$(HOME)/.local/state)/reel/log/reel.jsonl

run: ## launch the app (debug build)
	cargo run -p reel-tauri

test: ## engine + UI-logic tests — headless, fast, no GUI deps
	cargo test -p reel-core
	node tests/zone.test.mjs

logs: ## follow the app log (UI + engine, newest last)
	@tail -n 50 -F $(REEL_LOG)

# `grep .` so an empty result exits non-zero — piping into sort would otherwise
# mask it and the fallback message would never print.
errors: ## just the failures from this log, oldest first
	@grep -h '"lvl":"\(warn\|error\)"' $(REEL_LOG)* 2>/dev/null | sort | grep . \
		|| echo "no warnings or errors logged"

dump: ## print what the engine sees (trips + inserted card) as JSON
	cargo run -p reel-core --example dump

reproxy: ## rebuild already-cached proxies on the current recipe (see proxy.rs)
	cargo run -p reel-core --release --example reproxy

build: ## optimized release binary -> target/release/reel
	cargo build -p reel-tauri --release

fmt:
	cargo fmt

clean:
	cargo clean
