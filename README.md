# reel

A desktop app for managing trip footage from DJI / GoPro / iPhone cards: ingest
into per-trip workspaces, review and mark, cut, and push to a shared cloud.
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
make logs     # follow the app log
make errors   # just the warnings and errors from it
make build    # optimized release binary -> target/release/reel
```

### When something goes wrong

reel writes JSONL to `<XDG_STATE_HOME>/reel/log/reel.jsonl` (usually
`~/.local/state/reel/log/`) — UI and engine to the same file, in order, so a
misbehaving window leaves a trace rather than needing a reproduction. It rotates
at 2 MiB and keeps one previous generation. `make errors` is the fast way in; a
bug report is far more useful with those lines attached.

No npm or tauri-cli needed — the static frontend is embedded at compile time, so
a plain `cargo build` produces the shipping binary too (the AUR recipe in
`packaging/` builds exactly this way).

## Layout

A Cargo workspace (same shape as `hypr-tools`):

- **`crates/reel-core/`** — the engine. Pure Rust, **no GUI dependency**, so it's
  unit-tested headlessly (`cargo test -p reel-core`). All footage logic lives
  here: card/session survey, the content-id dedup ledger, trip discovery/state,
  and (as they're built) import, cut, cloud sync, wipe.
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
  The same player also **previews an inserted card** read-only — click a card
  session's contact strip to skim its clips (straight off the card, proxies
  cached by content id) before deciding to import.
- [x] **2 — Cut**: marked ranges → `clips/`, lossless (one ffmpeg stream-copy per
  mark, `-ss/-to` before `-i`, primary v+a only), with live per-clip progress on
  the trip card. Additive and re-runnable: existing clips are left untouched, so
  adding marks and cutting again only writes the new ones. **Edit** is the other
  way out: the trip's marks become one `.kdenlive` timeline against the masters,
  launched detached. Cut writes standalone files; Edit is where you go to work.
- [x] **3 — Import + session picker + cloud**: card→workspace copy + ledger write,
  then rclone push/verify to the shared cloud. Import is session-scoped with live
  progress, dedup, and capture-time preserved; **Share** uploads your masters,
  verifies them with `rclone check`, and only then records `share=shared` — so a
  trip reads ✓ Shared and its card sessions turn safe-to-clear on proof, never
  before.
- [x] **4 — Space**: reclaim the card (delete masters that are verified-imported
  **and** cloud-confirmed, per session or whole-card) and **archive** a trip (free
  its local raw once every master is in the cloud, keeping clips/marks). Both gate
  a live cloud check behind an explicit confirm; card deletes are guarded to card
  paths, and archive re-verifies before freeing the only local copies.
- [x] **5 — Packaging**: `reel-git` on the AUR (plain `cargo build` — the frontend
  embeds, so no tauri-cli/npm), a `.desktop` launcher + icons, and a
  `packages/arch.txt` entry so the dotfiles `make sync` installs it. Recipe and
  publish/update flow in `packaging/`.
- [x] **6 — Organize**: fix footage in the wrong trip and clear what you don't
  want, without breaking the dedup ledger or the cloud. **Move** clips between
  trips (in the player with `m`, or a multi-select **organize board** you drag onto
  a destination), **rename**/**merge** trips, and **permanently delete** clips or a
  whole trip — locally *and* from the cloud for your own footage, **tombstoned** so a
  copy still on a card reads "discarded" and never re-imports. **Pull** others'
  footage down from the cloud into a trip; a per-clip provenance badge plus the trip
  card show whose is whose. Every relocation keeps files, ledger, marks, cut clips,
  and cloud in step, and never claims a cloud safety it can't prove (an offline move
  drops the destination to "sharing unknown"). Engine in `organize.rs` / `remove.rs`
  / `pull.rs`, headlessly tested.
- [x] **7 — Sync**: replace the one-shot `share=shared` flag with a live, computed
  sync status per trip. A persisted **baseline of *intended* cloud contents** is
  diffed against local files and the live cloud listing to surface exactly what's
  owed — **to share**, **to pull** (footage others added), **remove from cloud**
  (an offline delete no longer forgets its cleanup — the leftover reads as a
  zombie to clear), **removed-upstream** orphans, and **conflicts** — in a **Sync
  panel**, with owed moves/renames/purges **queued and replayed** when the remote
  returns. Deriving deletion from *intent* (not local absence) means **archive**
  reads as in-sync rather than a phantom delete; "safe to clear" now comes from the
  baseline, so a post-share import stops falsely reading as safe; and **rename** no
  longer drags other contributors' cloud footage. Engine in `sync.rs` / `store.rs`,
  headlessly tested.
- [x] **8 — Per-trip sharing**: share one trip's cloud folder with specific people
  instead of exposing the whole cloud. A per-trip **Sharing panel** (⋯ → Sharing…)
  lists who a trip is shared with and adds/revokes friends, driving the **OCS Share
  API** from `share.rs`. The add box is a combobox: a **dropdown of people you
  already share with** (unioned network-free from the local share caches) plus live
  Nextcloud search as you type, so you rarely have to type a full username. Each pushed
  trip's card carries a one-click sharing chip that opens the panel — a quiet
  **🔗 Share…** when it's shared with nobody yet, brightening to **🔗 Shared · N**
  once you add people (so sharing a trip never means digging through the ⋯ menu).
  The state comes from a network-free per-trip cache every share op maintains, so
  the dashboard stays fast (a background sweep warms the chips on launch). No second
  login: the Nextcloud base URL, user, and password are reused from the rclone
  webdav remote (`rclone config dump` + `rclone reveal`), and requests go out
  through curl with the credentials fed on stdin (never argv). A non-Nextcloud cloud
  disables the panel with a reason; the parsing/URL logic is unit-tested.
  **Friend side:** set each collaborator's Nextcloud *Default share folder* to
  `Reels`, so a shared trip lands at `Reels/<trip>` where their
  `REEL_REMOTE=nextcloud:Reels` already looks — no per-trip reconfiguration.
- [x] **9 — Deduplicate**: a whole-library **duplicate finder** — the same clip in
  more than one trip, or a cloud folder orphaned by a reorg. Scans local footage
  (free) plus one recursive cloud listing, groups clips by content identity
  (`basename` + size), and prunes the extras down to one canonical copy — local
  *and* cloud — at your pick (a fully-synced copy in a named trip beats a cloud-only
  orphan). Unlike a permanent delete it **never tombstones** — the content lives on
  in the kept copy, so it stays importable — and never removes a copy it can't prove
  has a survivor (a local↔local prune re-checks the content id first). Engine in
  `dedup.rs`, headlessly tested; a topbar **Duplicates** panel drives it.

Poster-frame **thumbnails** (ffmpeg, cached by content id) and a **black-and-
white, per-trip-colour redesign** (see `PRODUCT.md`) landed ahead of Phase 5:
each trip gets its own colour, trip cards and card sessions lead with the
footage. Each trip also shows **what's yours vs. pulled from others** (by
`<trip>/<person>/` folder) and your **share status** — the latter from `.reel`'s
`share=` line, shown as "unknown" until a verified push records it.

The inserted card can be **previewed** before importing (click a session's contact
strip to skim it full-screen, read-only). **Photos are first-class captures**
alongside video — pictures and DJI **panoramas** import, cluster into sessions,
push, and clear exactly like clips (a panorama is imported as the drone's single
finished, stitched image, not its raw source frames; those frames are swept off the
card with it once it's safely inCloud). So a friend's mixed photo+video contribution
just works. The card can also be **cleared offline** (a subdued escape hatch) when
there's no internet to verify the cloud first — the footage then rests on its single
local copy, with a warning.

Import, Review, Cut, Edit, Share, card reclaim, and archive match the `reel`
script; reorganize (move/rename/merge), permanent delete, pull-from-cloud,
card preview, and mixed photo/video import go beyond it. Packaged on the AUR as
`reel-git` and installed by the dotfiles `make sync`.
