# reel

A desktop app for managing trip footage from DJI / GoPro / iPhone cards: ingest
into per-trip workspaces, review and mark, cut, and push to a shared pool.
Successor to the `reel` shell script (`bin/.local/bin/reel`), which stays usable
during the port.

## Install

Arch (AUR) — also pulled in by the dotfiles `make sync`:

```sh
paru -S reel-git
```

Puts `reel` on `PATH` and adds a launcher entry.

## Run it (from source)

```sh
make run      # launch the app (debug)   — needs only cargo (UI is embedded)
make test     # headless engine tests
make dump     # print what the engine sees right now (trips + inserted card)
make build    # optimized release binary -> target/release/reel
```

No npm or tauri-cli needed — the static frontend is embedded at compile time, so
a plain `cargo build` produces the shipping binary too (the AUR recipe in
`packaging/` builds exactly this way).

## Layout

A Cargo workspace (same shape as `hypr-tools`):

- **`crates/reel-core/`** — the engine. Pure Rust, **no GUI dependency**, so it's
  unit-tested headlessly (`cargo test -p reel-core`). All footage logic lives
  here: card/session survey, the content-id dedup ledger, trip discovery/state,
  and (as they're built) import, cut, pool sync, wipe.
- **`src-tauri/`** — thin Tauri v2 command layer over the engine.
- **`ui/`** — vanilla HTML/CSS/JS dashboard, no bundler.

The engine honours the same env knobs as the script (`REEL_LIB`, `REEL_REMOTE`,
`REEL_USER`, `REEL_SESSION_GAP`, `DJI_SD`/`GOPRO_SD`); see
`crates/reel-core/src/config.rs`.

## Roadmap

- [x] **0 — Foundation**: workspace, engine read-side (scan/sessions/trips/ledger)
  with tests, Library dashboard (trips + inserted card's sessions).
- [x] **1 — Review/Player**: a full-screen player skims each trip's clips
  (playing the camera's native `.LRF`/`.LRV` proxy, a cached 720p proxy, or the
  master — building one with ffmpeg on the fly when a master won't decode), with
  a timeline, filmstrip, and `i`/`o`/`h`/`u`/`e`/`x` keys. Marks key on the
  master and persist to `marks.tsv` byte-compatibly, so `reel cut` reads them
  unchanged. Clips stream from a loopback HTTP server with byte-range/seek —
  WebKitGTK's media backend can't load a custom URI scheme (WebKit bug 146351).
- [x] **2 — Cut**: marked ranges → `clips/`, lossless (one ffmpeg stream-copy per
  mark, `-ss/-to` before `-i`, primary v+a only), with live per-clip progress on
  the trip card. Additive and re-runnable: existing clips are left untouched, so
  adding marks and cutting again only writes the new ones. Opening a finished cut
  in an editor (Kdenlive) is the one step still left to the `reel` script.
- [x] **3 — Import + session picker + pool**: card→workspace copy + ledger write,
  then rclone push/verify to the shared pool. Import is session-scoped with live
  progress, dedup, and capture-time preserved; **Share** uploads your masters,
  verifies them with `rclone check`, and only then records `share=shared` — so a
  trip reads ✓ Shared and its card sessions turn safe-to-clear on proof, never
  before.
- [x] **4 — Space**: reclaim the card (delete masters that are verified-imported
  **and** pool-confirmed, per session or whole-card) and **archive** a trip (free
  its local raw once every master is in the pool, keeping clips/marks). Both gate
  a live pool check behind an explicit confirm; card deletes are guarded to card
  paths, and archive re-verifies before freeing the only local copies.
- [x] **5 — Packaging**: `reel-git` on the AUR (plain `cargo build` — the frontend
  embeds, so no tauri-cli/npm), a `.desktop` launcher + icons, and a
  `packages/arch.txt` entry so the dotfiles `make sync` installs it. Recipe and
  publish/update flow in `packaging/`.

Poster-frame **thumbnails** (ffmpeg, cached by content id) and a **black-and-
white, per-trip-colour redesign** (see `PRODUCT.md`) landed ahead of Phase 5:
each trip gets its own colour, trip cards and card sessions lead with the
footage. Each trip also shows **what's yours vs. pulled from others** (by
`<trip>/<person>/` folder) and your **share status** — the latter from `.reel`'s
`share=` line, shown as "unknown" until a verified push records it.

Import, Review, Cut, Share, card reclaim, and archive now work in the GUI; the
only step still on the `reel` script is opening a finished cut in an editor.
Packaged on the AUR as `reel-git` and installed by the dotfiles `make sync`.
