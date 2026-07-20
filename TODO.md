# TODO

## Phase 7 — Sync (live pool reconcile)

Goal: make it clear what happens when local footage or the shared pool drift out
of step. Replace the one-shot `share=shared` flag with a computed, reconcilable
sync status. See PRODUCT.md principles 1 (clarity) & 3 (earn the right to destroy).

### Model
- **Baseline** = what *should* be in the pool (per-trip `synced/<trip>.tsv`),
  maintained inline by every pool op. Status = set arithmetic over Local (disk),
  Baseline, and Remote (one `rclone lsjson`, cached).
- Load-bearing invariant: **deletion is driven by baseline (intent), never local
  absence** — so `archive` (frees local, keeps pool) reads as in-sync, while an
  offline delete (dropped from baseline, still in pool) reads as a zombie to clean.
- Owed **move / rename / purge** (can't be re-derived from the diff) go to a
  durable **pending queue** (`pending.tsv`) and replay on the next sync.

### Engine (reel-core, headlessly tested)
- [x] `store.rs` — `FileSet`, `Pending`/`PendingOp`, pool cache; atomic TSV I/O
- [x] `sync.rs` — `sync_status`, `reconcile`, `reconcile_all`, `trips_in_pool`,
      `brief` (network-free card status); L/B/R classification
- [x] baseline wired into `push` / `pull` / `delete_clips` / `delete_trip` /
      `move_clips` / `rename_trip`; `rename` pool op **scoped to your subtree**
      (was moving the whole trip folder — other contributors included)
- [x] `cards.rs` "safe to clear" derives from the baseline (post-share import is
      no longer falsely safe); reclaim's own per-file `rclone check` gate untouched
- [x] 9 new tests in `tests/engine.rs` (63 total green), clippy-clean

### Commands + UI
- [x] `sync_status` / `sync_trip` / `sync_all` (registered)
- [x] live status chip per trip card (to share / in pool / to clean / queued / in sync)
- [x] **Sync panel** (`#sync-dialog`): grouped drift + per-group opt-in actions,
      Refresh; a `☁ N in cloud` chip for footage in the pool but not on this machine
      (archived/freed raw). "Safe to clear"/Archive gates now read the baseline.
- [x] topbar **Sync** = full sweep: replay owed ops (incl. a purge from an offline
      trip-delete, which has no card to open), then fetch/diff/push/pull every trip
      (additive; deletions/conflicts stay per-trip opt-in). Progress panel
      (`#syncall-dialog`): overall "trip X of Y", the trip in flight, a per-trip log.

Build/verify: `cargo test -p reel-core` green · `cargo clippy --workspace` clean ·
`cargo build -p reel-tauri` embeds the UI and launches clean. Remaining: user review
of the running app.

### Decisions (locked)
- Scope: **A + B, single-writer** (mostly your own footage; others rarely). No
  trip-UUID/manifest (Phase C) — renames are effectively single-writer.
- Trigger: **manual** (per-trip Sync panel + a global replay), matching the
  existing explicit Import/Share/Pull.
- Offline moves reconcile as delete-and-recopy only if a queued move can't replay;
  the pending queue means the efficient server-side move is the norm.

### Known limitation (documented)
- Renaming a trip that contains *pulled* footage leaves that footage under the old
  pool name (we only move your own subtree). Acceptable under the single-writer
  scope; a trip-UUID/manifest (Phase C) is what would make it converge across peers.

## Phase 8 — Per-trip sharing (Nextcloud ACLs)

Goal: stop sharing the whole `Reels` folder with everyone. Share each trip's pool
folder with only its participants, managed from inside reel. rclone moves files
but can't set Nextcloud shares, so this drives the **OCS Share API** directly.

### Engine (`share.rs`, unit-tested)
- [x] Reuse the rclone webdav remote's credentials — `rclone config dump` for
      url/user/pass, `rclone reveal` to de-obscure the password, base URL = the
      part before `/remote.php`. No second login, no new secret storage.
- [x] `list_shares` / `add_share` / `remove_share` / `search_sharees` (GET/POST/
      DELETE `/ocs/v2.php/.../shares`, sharee autocomplete) via **curl**, auth fed
      on **stdin** (`-K -`) so `user:pass` never hits argv. `sharing_available`
      gates the feature (Nextcloud webdav + curl present) without revealing the pass.
- [x] Collaborator permissions (15 = read+update+create+delete, no reshare) so a
      friend can pull the trip and push their own footage but not re-share it.
- [x] Pure transforms tested headlessly: base-URL derivation, pool/trip paths,
      OCS envelope, share + sharee parsing, curl-config escaping (6 tests).

### Commands + UI
- [x] `sharing_status` / `trip_shares` / `share_add` / `share_remove` /
      `sharee_search` (registered), all off the UI thread.
- [x] **Sharing panel** (`#share-dialog`, ⋯ → Sharing…): lists who a trip is
      shared with, Remove per row, an add-a-friend box with Nextcloud autocomplete.
      Gated: a non-Nextcloud pool shows the reason instead of controls. Display
      names render via `textContent` (they can hold HTML-special chars).
- [x] **At-a-glance card chip**: every share op writes a per-trip cache
      (`state_dir/shares/<trip>.tsv`, `share_cache_path`), so `build_trip` surfaces
      `sharedWith` on the card **network-free** (like the pool cache feeds the sync
      chip). `cached_shares -> Option<usize>` uses the cache *file* (exists vs
      missing) as the "is it shareable" signal, giving a 3-state chip: `None` (not
      in pool / unchecked) → no chip; `Some(0)` (in pool, nobody yet) → a quiet
      **🔗 Share…** call-to-action; `Some(n)` → **🔗 Shared · N**. So an
      unshared-but-pushed trip gets a **one-click share button right on the card** —
      no ⋯-menu. Click/Enter opens the panel. A once-per-session background sweep
      (`warmShareChips`, gated on `sharing_status`, bails offline) lights the chips
      up without visiting each panel; opening a panel, adding/removing, or pushing a
      trip refreshes the chip live in place. `remove_share` now takes `trip` too, so
      it can prune the cache without a re-list.
- [x] **Friend dropdown** in the add box: `known_sharees` (engine) unions every
      per-trip share cache (`share_cache_dir`) into a network-free "people you share
      with" list, exposed as the `share_friends` command. The add-a-friend field is
      now a **combobox** (`#share-combo`): a ▾ opens a dropdown of those friends
      (minus whoever's already on the trip), live Nextcloud `sharee_search` layers on
      as you type, and picking one shares immediately. Replaces the WebKit-flaky
      `<datalist>`; arrow-key nav, Enter to pick/add, Escape closes the menu (not the
      dialog). Display names via `textContent`.

### Migration + friend setup (manual, one-time)
- To move off the blanket share: add friends to each trip via the panel, then
  **unshare the parent `Reels`** folder in the Nextcloud web UI. The panel shows
  *direct* shares on the trip folder (`reshares=false`), so a trip still covered
  only by the parent share reads as "not shared with anyone yet" until you add them.
- A trip must be **in the pool** (pushed) before it can be shared — its pool folder
  has to exist. Sharing a not-yet-pushed trip returns Nextcloud's "folder doesn't
  exist".
- Each friend sets their Nextcloud **Default share folder** to `Reels` so received
  trip shares land at `Reels/<trip>`, matching `REEL_REMOTE=nextcloud:Reels`.

Build/verify: `cargo test -p reel-core` (76 total green: 10 lib unit + 66 integration)
· `cargo clippy --workspace` clean · `cargo build -p reel-tauri` embeds the UI.
Remaining: user test against the live Nextcloud (creates real shares — not run
unattended).

### Not done / possible follow-ups
- Public-link shares and per-friend read-only (viewer) vs collaborator levels — the
  panel is user-shares + one permission level for now.
- The card chip's count is refreshed lazily (panel open + a once-per-session sweep),
  so it can lag a share made elsewhere until the next launch/panel-open — same
  cached-freshness trade as the sync chip's pool listing.

## Phase 9 — Global deduplication

Goal: find the same clip living in more than one place — copied into two trips, or a
pool folder orphaned by a rename/reorg — and prune the extras. Motivated by real
dogfooding drift: an early default-named trip (`2026-05-26_to_06-12`) got
reorganized into party/grad/knicks/DOHA, leaving a 17 GiB byte-for-byte duplicate
orphan in the pool. Complements import-time dedup (the ledger only stops a clip *on
a card* re-entering a trip; nothing caught a clip copied across trips).

### Engine (`dedup.rs`, headlessly tested)
- [x] `scan` — walk every local trip (free) + one recursive pool `lsjson`; group
      masters by identity `(basename, byte size)` — the same rel+size trust sync /
      push / pull already run on — and flag any identity at ≥2 distinct `(trip, rel)`
      locations. Depth-agnostic (handles `person/base` *and* `person/camera/base`),
      unlike `sync::rel_of`'s 3-segment requirement.
- [x] `resolve` — keep one canonical per group, prune the rest: unlink the local
      master (+ sidecars/proxy), `deletefile` the pool copy, drop the rel from the
      baseline + pool cache. **Never tombstones** (the content survives in the kept
      copy) — instead re-points the ledger row to the survivor.
- [x] Safety: never prune unless a survivor is provably present (canonical local
      file exists, or it's pooled and the remote is reachable); a local↔local prune
      re-checks byte identity by content id first, so a name+size collision can't
      cause a loss. Canonical heuristic prefers a fully-synced copy in a named trip
      over a pool-only copy in a date-range default.
- [x] 4 tests: cross-trip detection + canonical pick, prune (no tombstone, baseline
      drop), the last-copy guard, the content-collision guard.

### Commands + UI
- [x] `dedup_scan` / `dedup_resolve` (registered), off the UI thread; a
      `Channel<DupProgress>` streams per-copy prune progress.
- [x] Topbar **Duplicates** → `#dedup-dialog`: one card per group, a radio to pick
      the keeper (defaults to the suggested canonical) and a checkbox to include the
      group, total reclaimable, a prune progress bar. Trip/person/basename via
      `textContent`.

Build/verify: `cargo test -p reel-core` (80 green: 10 lib unit + 70 integration) ·
`cargo clippy --workspace` clean · `cargo build -p reel-tauri` embeds the UI.
Remaining: user test against the live pool (removes real files).

### Not done / possible follow-ups
- Identity is name+size (with a content-hash recheck before a local prune), not a
  full content hash of every clip up front — cheap and consistent with the rest of
  the app, but a pool-only↔pool-only prune trusts name+size alone.
- A pruned copy's cut clips (`clips/*`), if any, are left in place, not carried over
  to the survivor.

## Card preview (watch a card before importing)

Goal: click a card session's contact strip to skim its clips full-screen, before
committing to an import — reusing the trip player read-only.

### Engine (reel-core)
- [x] `Config::clip_roots()` — the clip server's allowed roots widen from just the
      library to include the card mount (`/run/media/<user>` + `DJI_SD`/`GOPRO_SD`).
      `serve::serve_clip` now takes a roots slice and admits any of them (still a
      whitelist of same-user mounts, never an arbitrary path).
- [x] `proxy::ensure_card_proxy` — the card twin of `ensure_proxy`: no trip, cached
      by content id under `cache_dir/proxies/<id>.mp4` (`Config::card_proxy_path`).
      Shared ffmpeg core (`build_proxy`) with the trip path.
- [x] `review::card_playlist(cfg, window)` — read-only `Playlist` for the inserted
      card, optionally scoped to one session's `[start, end]`; cheap `quick_fileid`
      ids (the card isn't on the ledger), no marks. `cards::card_masters` reused.
- [x] `ReviewClip.poster` — a **small** frame source (native `.LRF`/`.LRV` or a
      built proxy, else the master), set by both playlists. DJI card masters run
      8–10 GB; grabbing posters/filmstrip frames from the multi-GB master on slow
      card media was the real "very slow", so frames now come from the tiny proxy.

### Commands + UI
- [x] `card_playlist(start, end)` / `make_card_proxy(master)` (registered), off the
      UI thread.
- [x] Player **preview mode** (`P.preview`): `openCardPreview(window, label)` opens
      the same full-screen player with a neutral accent; edit keys (i/o/h/u/e/x/m/
      Delete) and the mark/move/delete chrome + the edit half of the key legend are
      gated off; proxies build via `make_card_proxy`. The card session's contact
      strip is a button (hover play badge, Enter/Space) → previews that session.
- [x] Preview **skips the `clip_health` ffprobe** (it would probe a multi-GB master
      on slow card media, and its overrun past the 15 s load-timeout was surfacing a
      false "corrupt or unsupported" error); the DJI clips are known-good and play
      via their `.LRF`. Posters/filmstrip use `poster`, never the master.

Build/verify: `cargo test -p reel-core` (81 green: 10 lib unit + 71 integration —
serve multi-root + card-proxy-in-scope regression cases added) · `cargo clippy
--workspace` clean · `cargo build -p reel-tauri` embeds the UI · `node --check
ui/app.js`. Remaining: user test with a real card inserted.

Field notes from the first live run (all fixed): the health probe/posters were
hitting the 8–10 GB masters (slow + a false "corrupt" from the load-timeout); and
card proxies cache under `cache_dir/proxies`, which had to join `clip_roots()` or
the loopback server 403s them ("won't play, even as a proxy").

## Share — informative progress, no self-contention, a walk-away queue

Sharing a 39 GiB trip read as "stuck at 0/4%", the progress vanished on a Rescan,
and sharing several trips meant babysitting each one to start the next.
- [x] `PushProgress` gains `speed` (bytes/sec) + `eta` (sec) from rclone's JSON
      stats; the readout shows **bytes done / total · rate · time left** (with a
      `fmtRate` that doesn't round a slow upload to "0 MiB/s"), not a bare %.
- [x] Root cause of the crawl: a session-level **Share** pushes the *whole* trip, so
      clicking it on each of a trip's 3 sessions launched 3 concurrent rclone copies
      of the same footage, fighting over the same bytes. One push now covers all a
      trip's sessions.
- [x] **Rescan-proof progress**: the live figures live in a module-state
      `shareProgress` Map keyed by trip (not a captured DOM node), and every card /
      session owned by an uploading trip renders a `data-share-trip`-tagged bar that
      `paintShareProgress` refills — so a re-render/Rescan mid-upload never orphans it.
- [x] **Share queue** (`shareQueue`, drained at concurrency 1): every share path — a
      trip card's Share, a session's "Share to clear", and a new **Share all** button
      in the Trips heading — feeds one queue, so you can queue a batch and walk away.
      Uploads run one at a time (bandwidth-bound), each showing its live bar in turn;
      a waiting trip reads a muted **Queued to share…** with a ✕ to drop it. A second
      click never fights the first push — it just enqueues (so the old "… already
      uploading" no-op is gone). UI-only, on the existing `share_trip` command;
      survives a Rescan (module state). The topbar **⇅ Sync** stays the push+pull
      superset for a full reconcile.

## Photos as first-class captures (a panorama is the one stitched image)

Supersedes the earlier "import the `PANORAMA/<seq>/` folders" approach, which had it
backwards. A DJI panorama's `PANORAMA/<seq>/` folder holds the **raw source frames**;
the drone *also* writes the **finished, stitched panorama** as an ordinary wide JPG
beside the videos (`DJI_<ts>_<seq>_D.JPG`) — that's the image the DJI app shows. reel
was importing the 4–9 negatives and ignoring the deliverable. Now **photos are
first-class captures** (a picture, or a stitched panorama, is just a capture with no
video stream), so the panorama imports as **one image** and a friend's mixed
photo+video contribution works end to end. Net change is a **deletion** — the parallel
panorama track (`PanoRec`, `Ev`, the second import loop, the `Panorama` model, the
pano UI) collapses into the one capture path.
- [x] `media`: `MASTER_EXT` → `VIDEO_EXT` + `PHOTO_EXT` + `CAPTURE_EXT`
      (`jpg/jpeg/png/heic/heif/webp/dng`) + `is_video`/`is_photo`. `masters_in` /
      `masters_under` / `card_masters` return **captures** and skip the `PANORAMA/`
      source-frame subtree (`is_pano_source`). pull/sync/dedup pool listings broaden
      to `CAPTURE_EXT`; proxy/cut/marks stay video-only.
- [x] `sessions`: one `ClipRec` list (a `photo` flag) — no more `PanoRec`/`Ev` merge.
      `Session` = `captures` + `photos` (subset) + `new_captures`; `Panorama` gone.
      `CardInfo` = `captures` + `photos`. `ImportResult` = `copied` + `photos`.
- [x] `import`: photos ride the one masters loop → `<trip>/<you>/<camera>/` (a DJI
      stitched pano → `dji/`), ledgered/deduped like a clip. The second loop is gone.
- [x] `proxy`/`review`: a photo is its own view — `ensure_proxy`/`ensure_card_proxy`
      no-op it; `ReviewClip.photo` tells the UI to show an `<img>` (poster = the photo
      itself; `thumbs` downsizes the JPEG), never a stub.
- [x] `wipe`: the stitched photo clears like any capture; its `PANORAMA/<seq>/` source
      frames are **swept with it**, but only once that photo is itself cleared-safe
      (imported + pool-verified, or an offline clear). `seq_num` maps
      `DJI_…_0039_D.JPG` ↔ folder `001_0039`; `commit_reclaim` tidies the emptied
      `PANORAMA/` too. So the card actually empties, and negatives are never deleted
      ahead of their deliverable (`source_frames_stay_until_their_stitched_photo_is_safe`).
- [x] UI: photos ride the normal (clickable) session strip; the player gets a **photo
      mode** (`<img>` on stage, no scrubber/marks/health/proxy build; `[ ]`+filmstrip
      to step; Move/Delete still work). Card header + session meta + import toast read
      "N clips · M photos". The pano-only strip/badge is deleted.
- [x] Tests: photos cluster with videos; a stitched pano imports as one photo and its
      frames sweep on reclaim; frames stay until the photo is safe. `cargo test -p
      reel-core` 74 integration green · clippy `--all-targets` clean · `node --check`.

Real card (`make dump`): **45 captures / 9 photos** (8 stitched panos + 1 plain photo;
source frames excluded) across 7 sessions — standalone panos as photo-only sessions,
the two Jul-2 panos folding into the japan video window as "13 clips · 2 photos".

### Not done / possible follow-ups
- HEIC/RAW that WebKitGTK can't render inline fall back to their ffmpeg-extracted
  poster rather than the full-res file — fine for review, not a full-res photo viewer.
- No slideshow / auto-advance through a run of photos — you step them manually.
- The sync panel lists photos in its drift buckets (they're captures now) but has no
  photo-specific affordances.

## Clear the card offline (no pool check)

For clearing a card with no internet. The engine's `plan_reclaim` already had an
`offline` flag (skips `verify_in_pool`, plans every verified-imported master —
footage then rests on the single local copy); it was just hardcoded `false`.
- [x] `plan_reclaim` command takes `offline: bool`.
- [x] Reclaim dialog: a **subdued "Clear without pool check"** fallback button
      (`#confirm-alt`) — `runDestructive` now waits on a go/alt/cancel choice and can
      switch the primary flow to the offline plan (own confirm + a single-copy
      warning). So a pool-unreachable reclaim isn't a dead end.
- [x] A **"Clear offline…"** escape hatch in the card safety bar whenever footage is
      imported-but-not-safe (the pooled path shows no Clear button there) — opens the
      whole-card offline wipe directly.

## Card panel — "Share to clear" dead-end (fixed)

An imported-but-not-pooled card session showed a **⚠ Share to clear** warning but a
no-op **Add to…** button ("lands in a later build"). Now the button is **Share →**
and pushes the session's owning trip(s) to the pool (same `share_trip` as a trip
card's Share, inline progress in the row); the re-scan then flips the session to
**✓ Safe to clear**. Extracted `shareProgressUI` / `bindShareProgress` so the trip
card and the session row share one push-progress readout.

### Not done / possible follow-ups
- Preview always starts at a session's first clip; no "start from this frame".
- Card-preview poster frames cache under `quick_fileid` (a separate entry from the
  contact strip's content-id posters) — a few extra small jpgs, not shared.

## Review pass — injection, a silent re-pull, a collaborator's pool copy

A critical read of the uncommitted work (engine + commands + UI). Five fixes below,
each behind a regression test or a build gate; everything else found is listed after.

- [x] **HTML injection → footage loss (critical).** `el(tag, cls, html)` assigned
      `innerHTML` and `csp` was `null`, so a name out of the shared pool could inject
      markup that runs with `invoke` access (`delete_trip`, `delete_clips`,
      `commit_reclaim`). A collaborator's `person/` folder called
      `<img src=x onerror=…>` fired on every dashboard load. `el`'s third arg is now
      **text** (`textContent`); the places that genuinely need markup use a new
      `elHTML`, and `esc()` covers the remaining HTML templates (`#danger-body`, the
      reclaim summaries). Flipping the default also closed vectors that had never
      been patched by hand — `sync-group-items`, `org-who`/`org-name`, `pull-name`
      and `trip-name` all carry pool-derived strings.
- [x] **CSP** set (`script-src 'self'`, no `unsafe-inline`) as the backstop, in
      Tauri v2's documented shape — `connect-src ipc: http://ipc.localhost` is
      developer-supplied, Tauri appends its own `script-src` hash.
- [x] **A permanently-deleted *pulled* clip came back on the next Sync.**
      `delete_clips` dropped every clip from the baseline, including a friend's —
      whose pool copy legitimately stays. The compare then read `L✗ B✗ R✓` → "to
      pull", and `reconcile_all` (topbar ⇅ Sync) pulls without asking, so the clip
      silently re-downloaded. The baseline drop is now **yours only**; a pulled clip
      reads as cloud-only, which sync never re-pulls.
- [x] **Dedup could delete a collaborator's pool footage.** `resolve` deleted any
      `pooled` copy regardless of owner, unlike `remove.rs` ("the pool copy is theirs
      to keep"). Now guarded on `person_of(rel) == user`, reported as `kept_pool`,
      and the rel keeps its baseline row (dropping it would re-pull the clip).
- [x] **Player shortcuts stayed live under an open modal** — `i`/`o` silently added
      and auto-saved marks behind the Move/Delete dialog, bare Delete dropped the
      selected mark, and a second `m` threw on `showModal()`. Added the
      `dialog[open]` guard the Organize handler already had.
- [x] **`thumb` / `review_playlist` / `save_marks` ran on the UI thread** (sync Tauri
      commands). `thumb` shells out to ffmpeg — from a multi-GB master when a clip has
      no proxy — which also defeated the frontend's `THUMB_MAX` throttle, since the
      calls could only queue on the main thread. All three are now `async` +
      `spawn_blocking` like their siblings.

Build/verify: `cargo test -p reel-core` **86 green** (10 lib unit + 76 integration;
2 new regression tests, each confirmed to fail with its own fix reverted) · `cargo
clippy --workspace --all-targets` clean · `cargo build -p reel-tauri` OK · `node
--check ui/app.js`.

Field notes from the live run (both fixed): the CSP allowed the loopback clip server
in `media-src` but **not `img-src`**, so video played while every photo failed with
"won't load" — the player's photo mode does `photoEl.src = clipUrl(...)`, the same
origin `video.src` uses. Both `clipUrl` sinks are covered now. Separately, `m` (move
clip) left an "m" in the trip-pick input: the keypress's default action ran after the
dialog focused its text box. Pre-existing, not from the modal guard — `case "e"`
already had the `preventDefault` and `case "m"` was missed; `e` and `m` are the only
player keys that focus an input.

### Found, not yet fixed
- `wipe`'s panorama sweep keys on the 4-digit DJI seq alone, so two panos sharing a
  seq let a cleared one authorise deleting the other's source frames.
- `trip_dirs` discovers depth-2 trips, but every op rebuilds the path as
  `lib.join(bare_name)` and keys state by bare name — a nested trip misresolves.
- The loopback clip server has no token beyond its ephemeral port.
- `start.zip(end)` silently degrades a half-specified window to "whole card"
  (latent footgun for `clear_discarded`).
- `list_trips`/`scan_card`/`clip_health` swallow task errors — a corrupt ledger
  renders as an empty dashboard rather than an error.
- `rclone reveal <obscured>` puts the reversible pool password on argv, though the
  curl path deliberately keeps `user:pass` off it.
- Photos that aren't jpg/png (heic/heif/webp/dng) get no poster: `thumbs` gates the
  direct-frame path on 3 extensions instead of `is_photo`, so they take the video
  seek path, which writes nothing for a single frame.
- `build_trip` hashes the cover (~8 MiB read) per trip on every `list_trips`.
- Cut progress uses captured DOM refs, so a Rescan mid-cut orphans the bar and
  re-enables the button — a second concurrent `cut_trip`.
- Bulk/keyboard delete confirms name no targets, only a count; Organize's keyboard
  path falls back to the focused (possibly off-screen) clip.

## Lost updates on the shared state files (the concurrency race)

Every Tauri command runs on its own `spawn_blocking` thread with no coordination —
no mutex anywhere in `src-tauri`, and each command builds a fresh `Config`. The
ledger, tombstones, owed-op queue and per-trip baselines are all read-modify-write
TSVs, so a background sweep and a user action interleave freely. The temp-sibling
write made each save *atomic* (never torn) but did nothing about a **lost update**:
both sides load, both mutate, the last save wins. Measured, not theorised — a
24-way concurrent tombstone insert lost **14 of 24** rows before the fix.

A dropped baseline row self-heals on the next refresh. A dropped **tombstone** does
not: the clip stops reading as "discarded" and deleted footage is offered for
import again. A dropped **queue row** strands an owed pool op forever.

- [x] `store::state_guard()` — one process-wide `Mutex` for the state files,
      poison-tolerant (every write is atomic, so refusing later writes would turn one
      failed op into a dead app).
- [x] `Ledger::update` / `Tombstones::update` / `FileSet::update` / `Pending::update`
      — load → mutate → save under that lock. The contract: do the slow work
      (copy, ffmpeg, rclone) **first**, then merge only the resulting edits inside.
      The lock is never held across IO, so a 39 GiB push can't block the dashboard.
- [x] Every call site restructured to that shape, so nothing holds state across its
      slow leg: `import` collects rows across the copy loop, `delete_clips` collects
      ids across its per-clip pool deletes, `move_clips` across a possible
      cross-device copy, `dedup::resolve` across its pool deletes, and
      `sync_status` replays its baseline edits (adoptions included) against fresh
      state after the pool listing.
- [x] `reconcile` / `reconcile_all` no longer rewrite the whole queue after replaying
      — they drop **only the ops that landed**, re-read under the lock, so an op
      queued by another command mid-replay survives (it used to be erased).
- [x] Unique temp siblings (`<name>.<pid>.<n>.tmp`) everywhere, replacing the fixed
      `.tmp` / `.partial` names — two writers sharing one temp could publish each
      other's half-written body, or fail the rename outright. Covers `marks.tsv`
      and `.reel` too. Strays are cleaned up on a failed rename.
- [x] `known_sharees` skips non-`.tsv` files, so a stray temp can't be parsed as
      share rows.

Build/verify: `cargo test -p reel-core` **87 green** (10 lib + 77 integration),
including a 24-thread regression test confirmed to fail (14/24 rows lost) with the
guard removed · `cargo clippy --workspace --all-targets` clean · `cargo build -p
reel-tauri` OK.

### Still open here
- `marks.tsv` is only protected against a torn write, not a lost update: the player
  autosaving (`save_marks` rewrites the whole list by design) can still race the
  read-filter-write in `move_clips` / `delete_clips` and resurrect a moved or
  deleted mark. Wants the same `update`-style helper.
- The pool and share caches are left unserialised on purpose — they're caches, and a
  lost write just means a stale listing until the next refresh.

## Player: an unplayable clip you can act on, and zoom that zooms the clip

Two things the live run turned up.

- [x] **"has no video" was a dead end.** An unfinished camera file (a few KB, no
      streams — a real one turned up in `ha-giang`: 1241 bytes, zero streams) showed
      only *"Empty clip — X has no video."* with nothing to do about it, while the
      *load-failure* overlay offered **Build a proxy** — which cannot work when there
      are no streams to remux. `showStageNote` now leads with what the file actually
      is (`1.2 KB — the camera never finished writing this one. There's nothing to
      recover.`, shown under the engine's own reason when the clip is below the same
      `512 KiB` stub threshold `review.rs` uses) and offers **Delete it**, gated off
      in card preview (read-only). The player already skipped these on open via
      `firstPlayable`, so this is only what you see when you pick one deliberately.
- [x] **A touchpad pinch zoomed the whole app.** A pinch reaches the webview as
      ctrl+wheel, and the webview's default is to scale the entire UI — useless
      mid-review and fiddly to undo. Ctrl+wheel is now swallowed document-wide (plus
      WebKit's `gesture*` events) and the **clip** scales instead: pinch to zoom
      about the cursor, drag to pan, plain two-finger scroll pans once zoomed,
      double-click returns to fit, and every clip change / player close resets to
      fit. 1×–8×, panning clamped to the stage. The `<video>` toggles play on click,
      so a drag that ends over it would have paused the clip — a capture-phase click
      handler swallows that when the pointer actually moved (>3 px).

Build/verify: `node --check ui/app.js` · `cargo build -p reel-tauri` OK. Zoom and
the delete action are UI-only; no engine change, no test coverage (they want a
pointer).

## Pinch zooms the clip (the webview had to be intercepted), clickable sync chip

- [x] **Two wrong levers before the right one.** Worth writing down, because both
      failures looked plausible:
      1. *Document-level ctrl+wheel `preventDefault`.* On WebKitGTK a touchpad
         pinch arrives as `GDK_TOUCHPAD_PINCH` and never becomes a DOM event, so
         there was nothing to cancel.
      2. *Watching the WebView's `zoom-level`.* WebKit scales the page internally
         without going through the GObject setter, so `notify::zoom-level` stays
         silent. This is the one that looked right and shipped broken.

      The lever that works is GTK's propagation order. WebKit claims the gesture
      with a `GtkGestureZoom` of its own — confirmed, not guessed:
      `nm -D -u /usr/lib/libwebkit2gtk-4.1.so.0 | grep gesture` lists
      `gtk_gesture_zoom_new`. A controller in the **capture** phase runs before the
      target widget's own controllers and before its `event` handler, and a
      `GtkGesture` that recognises the sequence reports the event handled, which
      stops propagation. So `pin_page_zoom` puts its own capture-phase GestureZoom
      in front of WebKit's and forwards `scale-changed` as a `pinch-zoom` event.
      WebKit never sees the gesture. `zoom-level` is still pinned as a backstop for
      ctrl+= and anything driving the property directly.
      - `gtk = "0.18"` and `webkit2gtk = "2"` must match what tauri builds against
        or `inner()` returns a type from a *different* crate instance;
        `cargo tree -i gtk` / `-i webkit2gtk` must each show one version (0.18.2 and
        2.0.2, shared with tauri / tao / muda / wry).
      - `add_events(TOUCHPAD_GESTURE_MASK)` is required — GtkGesture only sees
        touchpad gestures if the window asks for them.
      - GTK3 widgets hold only a **weak** pointer to their controllers, so the
        gesture is `mem::forget`ed; dropping the handle would silently unhook it.
      - `REEL_ZOOM_DEBUG=1` logs what GDK actually delivers, since this is the one
        path that can't be checked without a touchpad.
- [x] **The sync chip is now the way in.** It already named the drift ("10 to
      clean", "3 to share") but you had to go find the `Sync…` button — which isn't
      on every card. Clicking (or Enter/Space on) the chip opens the Sync panel for
      that trip, mirroring the share chip. `✓ In sync` stays inert.

- [x] **The Sync panel's group labels said the wrong things.** "New in the pool ·
      download" and "Remove from pool · remove" mixed a *state* with an
      *imperative*, so neither said which side of the sync the clips were on or
      what the tickbox was about to touch — "remove" in particular read as the
      mildest of the three when it's the only destructive one. Each group now gets
      a short chip naming what the clips *are* ("New from others", "Deleted here,
      still in the pool") plus a line spelling out the consequence, and the
      deletion verb is "delete for everyone".
      - The hint carries the **byte total**, which was missing entirely: `download`
        is ticked by default, and "· 250" gave no clue there were 100+ GiB behind
        it.

## No way back from the cloud, and "pool" vs "cloud"

- [x] **`cloud_only` had no download — the promise was never implemented.** The
      model called it "re-downloadable" in three separate comments and nothing
      implemented the download. So `archive` (free your raw once it's verified up)
      and clearing a pulled clip were both **one-way doors**: 145 GiB archived, no
      way back short of hand-running rclone. New `restore.rs` + a `restoreCloud`
      action on the Sync panel closes it.
      - File-by-file via `rclone --files-from`, not a folder copy like `pull` — a
        folder copy would also drag back the clips you deliberately cleared.
      - **Opt-in, and deliberately excluded from global Sync.** It re-fills disk you
        freed on purpose; a Sync All that silently pulled every archived master back
        would undo an archive in one click. `archived_footage_can_be_brought_back_down`
        asserts a plain Sync leaves the freed disk freed.
      - Counts what actually landed rather than what it asked for — a clip the pool
        lost between listing and copy shouldn't report as restored.
      - `restore_refuses_to_climb_out_of_the_trip` covers the rel-path check; the
        rels are engine-derived in `reconcile`, but `restore` is a `pub fn` the
        command layer can reach, so the list crosses the IPC boundary.
- [x] **The UI says "cloud", the engine says "pool".** Same thing, one word each
      side, and the boundary is deliberate — noted at `syncChip` in app.js. The
      fields arriving over IPC (`lastPoolCheck`, `poolSynced`, `keptPool`, `pooled`,
      `poolOk`, `scannedPool`) keep `pool`: they're the wire format, and renaming
      them for prose consistency would churn serde on both sides for nothing.
      - The rename collided in one place worth remembering: `toPull` and `cloudOnly`
        both became "in cloud" on the trip card, though one is footage new to you
        and the other is footage you had and freed. Now "↓ N new" and "☁ N cloud
        only".

## 303 phantom "cloud only" clips — the reader was stricter than the writer

- [x] **`sync::rel_of` demanded `person/camera/base`; `push`/`pull` write
      `person/base` too.** A friend who uploads straight into their folder with no
      camera level produces 2-component rels. `pull` copies that shape down
      verbatim and writes it into the baseline; `remove.rs` reads it fine. Only
      `sync::rel_of` rejected it — so those clips were never in **L**, and
      `L✗ B✓ R✓` classified them **cloud-only forever**: sitting on disk the whole
      time, permanently reported as missing, impossible to clear.
      - On a real trip: 303 of 533 baseline rows were 2-component (104 Tyler + 198
        jack + 1 mine), and the panel showed exactly "303 cloud only". Every one of
        the 533 was present on disk.
      - Now requires only the **owner** segment. Derived dirs stay out because
        `masters_in` already excludes `clips`/`_sheets`/`.proxies` at any depth —
        the component count was never what protected that.
      - `restore` reported "303 brought back" *truthfully* — it counts files on
        disk, and they were on disk. The count was right; the classification was
        wrong. Worth remembering when a summary and a status disagree.
- [x] **`toPull` vs `cloudOnly` now say which is which.** Both are "in the cloud,
      not here"; the difference is whether you've *had* it. "New from others" has
      never been on this machine (ticked by default — it's the point of sharing);
      "Freed here, still in the cloud" is footage you archived or cleared (never
      ticked by default — you freed that disk on purpose). That also parallels
      "Deleted here, still in the cloud", where the difference is intent: freed
      keeps the baseline row, deleted drops it.

## pool → cloud, engine included

- [x] One word everywhere now: identifiers, doc comments, README, and the IPC
      field names (`keptPool`→`keptCloud`, `poolOk`→`cloudOk`,
      `poolSynced`→`cloudSynced`, `pooled`→`inCloud`, `lastPoolCheck`→
      `lastCloudCheck`, `scannedPool`→`scannedCloud`, `removedPool`→
      `removedCloud`, command `pool_contributors`→`cloud_contributors`).
      - A serde rename fails *silently* — the field just reads `undefined` in JS.
        Both sides were cross-checked name-by-name after the rename, not just
        compiled.
      - `cloud_cache_path` moved from `<state>/pool/` to `<state>/cloud/`. It's a
        pure cache (last remote listing + stamp), so the only cost is one
        "not checked yet" per trip until the next Sync. The baselines in
        `<state>/synced/` — the load-bearing state — are untouched.

Build/verify: `cargo clippy --workspace --all-targets` clean · `cargo test -p
reel-core` 90 green · `cargo build -p reel-tauri` OK · `node --check ui/app.js`.
All three new engine tests were confirmed to fail with their fix reverted.
The pinch path can't be tested headlessly — it needs a touchpad. **Confirmed
working on hardware 2026-07-20**: the clip scales, the page doesn't. Two-part
mechanism, both halves required — a capture-phase `GestureZoom` turns the pinch
into a scale factor, and the `event` handler returns `Propagation::Stop` to kill
the emission before WebKit's class closure. The capture-phase claim alone did
*not* stop propagation; that was only settled by the debug log showing every
pinch still arriving at `event`.

## Play sometimes restarted the clip instead of pausing

Reported as intermittent, and the "sometimes" was the tell: the transport stayed
live during the two awaits between *picking* a clip and *attaching* its source.

`loadClip` sets `P.i` first, then pauses the element, then awaits — the `ffprobe`
health check on an unchecked clip, and for a clip with a native LRF/LRV a whole
`make_proxy` build, which can run for minutes. Across that gap `curClip()` and
the whole UI (header, filmstrip, marks) already point at the new clip while
`<video>` still holds the **previous** one. And that previous clip is normally
`ended`, because auto-advance is what got us here.

`play()` on an ended element seeks back to the start — that's the HTML spec, not
a WebKit quirk. So a play/pause press in that window resumed the clip you'd just
left, from frame 0, under a header naming a different clip and sometimes behind
the "Preparing…" overlay. It read as "I pressed pause and it restarted."

Fix: `P.srcFor` records which master the element is actually holding — set in
`playSrc`, cleared the moment `loadClip` disowns the old source. `videoReady()`
compares it against `curClip()`, and every control that touches the element
(play/pause, nudge, Home/End, shuttle, scrub, go-to-mark, play-zone) checks it.
Photos and stubs never set it, so the transport is inert there too.

Same pass, same class of bug: `playZone`'s one-shot `canplay` listener only
removed itself if it fired. A zone on a clip that never became playable (stub,
photo, failed proxy build) left it attached, and `canplay` fires again after any
re-buffer — so it would later yank the playhead into a zone belonging to a clip
long since left. Now at most one arm exists and `clearZone()` drops it.

Still by design: on the **last** clip of a trip auto-advance has nowhere to go,
so the clip genuinely ends and play restarts it. That's what every player does.

### Second cause, same symptom: Space during a reverse shuttle

A sweep for other restart paths turned up an independent one that produces the
identical complaint. `shuttle(-1)` (J) holds the element **paused** and walks
`currentTime` backwards on a 60 ms timer — the picture moves, but `video.paused`
reads true. So Space fell through `togglePlay`'s play branch and ran *forward*
instead of stopping; and if the rewind had already reached the head (the timer
pins `currentTime = 0` and stops), it played from 0 — a literal restart, from a
press the user meant as "stop". `togglePlay` now treats a live reverse scrub as
playing and just stops it, like K.

Also tightened while there:
- `playZone`'s `start()` re-checks that the element holds the clip the zone
  belongs to. It's armed off `canplay`, which re-fires after any re-buffer, not
  only on a fresh load.
- `stopShuttle` no longer writes `playbackRate` when it's already 1. `togglePlay`
  calls it on every press, and on GStreamer a rate write is a pipeline operation.
  Precautionary — not demonstrated to cause anything.

### Still open (found in the same sweep, deliberately not changed)
- `togglePlay` is the only transport path that doesn't `clearZone()`, so pausing
  a loop zone and resuming keeps looping. That's what a loop should do; the real
  wart is `currentZone()` falling back to the clip's *last* mark, which can arm a
  zone the user never asked for.
- `updateTime()` is a render function that seeks as a side effect (the loop-zone
  wrap). `markIn` calls it, so pressing `i` past `zoneEnd` moves the playhead.
- `ended` with no next playable clip does nothing at all — no state reset. Play
  then restarts the clip, which is standard, but `nextPlayable` also returns -1
  when every *remaining* clip is a stub, so it can happen mid-trip.
- `#player-proxy` calls `prepareThenPlay` directly, bypassing `loadClip`'s
  `clearZone()`/`videoReady()`.

### The actual reported bug: the trip cover kept keyboard focus

The two causes above are real, but neither was what the user hit. Their report —
"press space, it restarts; let it advance to the next clip, press space, it goes
back to the **first** clip; I fixed it by clicking the app" — points at focus,
not playback. Nothing in the transport restarts a *trip*.

`cover.tabIndex = 0` with an `onkeydown` that opens the trip on Enter/Space
(app.js ~597). The dashboard stays in the DOM behind the player, so the cover you
clicked to get in still holds focus. Its handler runs in the **target** phase —
before the player's document-level one — so every Space re-ran `openReview`,
which always starts at `firstPlayable()`. Hence "back to the first clip", and
hence clicking anywhere in the player (moving focus off the cover) "fixing" it.

Fix: `parkFocus()` blurs the active element as a full-screen view takes over, and
`openReview` / `openCardPreview` / `openOrganize` refuse to re-enter when already
open. The blur is synchronous, so it lands before the playlist await and a second
press can't repeat the open either. A card's session strip (~430) had the
identical shape, feeding `openCardPreview`.

Lesson worth keeping: "I fixed it by clicking" is a focus symptom. Three rounds
were spent on the playback state machine because the report was read as a
transport bug.

### No logging anywhere (why this took three rounds)
Four `eprintln!` in `main.rs`, zero in the engine, zero `console.*` in the UI, and
nothing written to a file. Every diagnosis here came from reading code and asking
the user to reproduce. This is the strongest argument for a real log.

## An app log (`log.rs`)

Three rounds of guessing at the play/pause bug, with nothing on disk to consult,
made the case. reel now writes JSONL to `<state_dir>/log/reel.jsonl` — UI and
engine to the same file, in order, so a JS failure and the Rust fault behind it
read as one story.

Design points that are load-bearing rather than taste:
- **Its own mutex, not `store::state_guard()`.** Engine code logs from inside
  state writes that already hold that guard, and `std::sync::Mutex` isn't
  reentrant — sharing it would deadlock on the first such line.
- **Records built through `serde_json`, not `format!`.** A panic payload or a JS
  stack trace contains newlines; one raw newline splits a record and corrupts
  every downstream reader of the format.
- **Every error swallowed, deliberately.** The one place in reel where that's
  right: a logger that propagates turns a cosmetic fault into a broken feature,
  and one that panics takes the app down over a full disk.
- **Bounded**: rotates at 2 MiB keeping one generation, so ~4 MiB worst case.
- **No new dependencies** — ISO-8601 is ~12 lines of Hinnant civil-calendar
  maths rather than pulling in chrono for one format string.

Wired up:
- `main.rs` inits before anything can fail, logs a start line with version + lib.
- The three commands that silently swallowed a panicked task now log it:
  `list_trips` (rendered as an empty dashboard), `scan_card` (as "no card"),
  `clip_health` (as a clip that just won't load). Behaviour is unchanged — they
  still degrade gracefully — but the reason is now recoverable.
- UI: `window.onerror` + `unhandledrejection` (the way a failed `invoke` used to
  vanish completely), every `toast` at info, every `showStageError` at error, and
  a line when Review opens with the clip/mark counts.
- `make logs` / `make errors`, and a README section.

4 unit tests, all confirmed to fail with their fix reverted (rotation dropped →
the oversized file is never set aside; escaping weakened → the record splits).

### Next, if the log earns its keep
- Nothing prunes `reel.1.jsonl` on uninstall.
- `thumb` still swallows its join error (returns None → placeholder). Lower
  stakes than the other three, but the same shape.
- No session id, so interleaved runs are only separable by the start line.

## `person/file` clips: the filename became a directory

Same class as `sync::rel_of`, found while that one was fresh, but this one
reshapes the library on disk rather than just misreporting.

`organize::person_camera` read the **second** path component as "the camera" —
the shape reel's own import writes (`person/camera/base`). But `pull` copies a
friend's cloud folder down verbatim, and a friend with no camera level has
`person/base`. For those, the *filename* was taken as the camera, so a move
built `alice/CLIP_0050.MP4/CLIP_0050.MP4`: a directory named after the clip with
the clip inside it, that bogus rel written into the baseline, and the same
nesting replayed at the cloud via `rclone moveto`. Every one of the 303 pulled
clips from Tyler and jack is this shape.

It also silently truncated anything *deeper* than two levels onto
`person/camera`, which can land two distinct clips on one destination path.

Fixed by taking the master's whole parent directory instead of two fixed levels
(`owner_and_subdir`), so the subpath rides along at whatever depth it arrived in.
`None` for a stray at the trip root, which has no owner and no provenance.

Two more readers of the same "first component is the owner" assumption were wrong
for a stray at the trip root, where the first component *is* the filename:
- `trips::person_of` — its doc already claimed `None` for a stray while returning
  `Some(filename)`, which put a filename in the contributor list. Fixed.
- `review.rs` — showed a filename as the provenance badge. Fixed.

2 tests, both confirmed to fail against the old two-level logic.

### Deliberately not fixed: `remove::rel_of`
Same shape, but its caller does `else { continue }`, so returning `None` for a
stray would make that clip **undeletable** — a worse failure than a wrong label.
Correct fix is to handle a stray explicitly (delete locally, no cloud path)
rather than to tighten the reader. Left alone until that's designed.
