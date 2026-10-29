# reel — Tauri footage manager. See README.md.
# The frontend is static and embedded at compile time, so `cargo run` is all you
# need to launch the app; npm/tauri-cli only come in for release bundling.

.PHONY: help run test test-live lint build install uninstall package fmt clean dump logs errors perf reproxy

PREFIX  ?= $(HOME)/.local
DATADIR ?= $(or $(XDG_DATA_HOME),$(HOME)/.local/share)
REEL_LOG := $(or $(XDG_STATE_HOME),$(HOME)/.local/state)/reel/log/reel.jsonl

help: ## this list
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | expand -t18

run: ## launch the app (debug build)
	cargo run -p reel-tauri

test: test-live ## engine + UI-logic tests — headless, fast, no GUI deps
	cargo test -p reel-core
	node tests/zone.test.mjs

test-live: ## the install launcher never runs a failed build
	scripts/test-reel-live

lint: ## clippy with warnings as errors, then a format check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --check

logs: ## follow the app log (UI + engine, newest last)
	@tail -n 50 -F $(REEL_LOG)

# `grep .` so an empty result exits non-zero — piping into sort would otherwise
# mask it and the fallback message would never print.
errors: ## just the failures from this log, oldest first
	@grep -h '"lvl":"\(warn\|error\)"' $(REEL_LOG)* 2>/dev/null | sort | grep . \
		|| echo "no warnings or errors logged"

perf: ## slow work (100 ms or more) from this log, oldest first
	@grep -h '"src":"perf"' $(REEL_LOG)* 2>/dev/null | sort | grep . \
		|| echo "nothing slow logged"

dump: ## print what the engine sees (trips + inserted card) as JSON
	cargo run -p reel-core --example dump

reproxy: ## rebuild already-cached proxies on the current recipe (see proxy.rs)
	cargo run -p reel-core --release --example reproxy

build: ## optimized release binary -> target/release/reel
	cargo build -p reel-tauri --release

install: ## live launcher in ~/.local for this user (no sudo)
	# Each launch runs the checkout's last build and rebuilds the tree
	# for the next one. @ROOT@ becomes the checkout that installed it.
	# The desktop entry gets the absolute launcher path: desktop
	# launchers may not have ~/.local/bin on PATH.
	install -d $(PREFIX)/bin
	sed 's|^DEFAULT_ROOT="@ROOT@"|DEFAULT_ROOT="$(CURDIR)"|' \
		scripts/reel-live > $(PREFIX)/bin/reel
	chmod 755 $(PREFIX)/bin/reel
	mkdir -p $(DATADIR)/applications
	sed 's#^Exec=reel#Exec=$(PREFIX)/bin/reel#' packaging/reel.desktop \
		> $(DATADIR)/applications/reel.desktop
	install -Dm644 src-tauri/icons/128x128.png \
		$(DATADIR)/icons/hicolor/128x128/apps/reel.png
	install -Dm644 src-tauri/icons/32x32.png \
		$(DATADIR)/icons/hicolor/32x32/apps/reel.png
	update-desktop-database $(DATADIR)/applications 2>/dev/null || true
	@echo "installed. reel launches from $(PREFIX)/bin/reel"

uninstall: ## remove the user install (footage, config and logs stay)
	rm -f $(PREFIX)/bin/reel \
	      $(DATADIR)/applications/reel.desktop \
	      $(DATADIR)/icons/hicolor/128x128/apps/reel.png \
	      $(DATADIR)/icons/hicolor/32x32/apps/reel.png
	update-desktop-database $(DATADIR)/applications 2>/dev/null || true

package: ## build the Arch package into packaging/ (in a scratch copy)
	# makepkg writes the pkgver() result back into whatever recipe it
	# runs (battery-log gotcha #7), so it gets a throwaway copy of the
	# PKGBUILD and only the finished package moves back into packaging/.
	# The recipe builds the pushed main from GitHub, not this tree.
	tmp=$$(mktemp -d); \
	trap 'rm -rf "$$tmp"' EXIT; \
	cp packaging/PKGBUILD "$$tmp/"; \
	(cd "$$tmp" && makepkg -f) && \
	mv "$$tmp"/*.pkg.tar.zst packaging/ && \
	ls -1t packaging/*.pkg.tar.zst | head -1

fmt: ## format the code
	cargo fmt

clean: ## remove build output
	cargo clean
