//! Tauri commands — a thin, serializing layer over reel-core. Each builds a
//! fresh Config from the environment so the GUI honours the same env knobs.

use reel_core::{
    ArchivePlan, ArchiveProgress, ArchiveResult, CardInfo, Config, Contributor, CutProgress,
    CutResult, DeleteResult, DupProgress, DupReport, DupResolution, DupResolveResult,
    ImportProgress, ImportResult, Mark, MoveResult, Playlist, PullProgress, PullResult,
    PushProgress, PushResult, ReclaimPlan, ReclaimResult, Sharee, SyncActions, SyncProgress,
    SyncResult, Trip, TripShare, TripSync, WipeProgress,
};
use std::path::Path;
use tauri::ipc::Channel;

/// The clip server's base URL (`http://127.0.0.1:<port>`), managed as app state
/// so the UI can build `<video>` URLs against it (see `clip_server`).
pub struct ClipBase(pub String);

/// Return the loopback base URL the UI appends clip paths to. The `<video>`
/// player fetches this once, since a custom URI scheme can't stream media on
/// WebKitGTK (WebKit bug 146351).
#[tauri::command]
pub fn clip_base(base: tauri::State<'_, ClipBase>) -> String {
    base.0.clone()
}

/// The trip list for the dashboard. Async + off-thread: building each trip walks
/// its footage (and now reads its sync baseline), so it must never run on the UI
/// thread. Runs concurrently with `scan_card` (the frontend awaits both together).
#[tauri::command]
pub async fn list_trips() -> Vec<Trip> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::list_trips(&cfg)).await {
        Ok(t) => t,
        Err(join) => {
            // Was a bare `unwrap_or_default()`: a panicking walk rendered as an
            // empty dashboard, indistinguishable from having no trips at all.
            reel_core::log::error("tauri", &format!("list_trips failed: {join}"));
            Vec::new()
        }
    }
}

/// The inserted card's sessions. Async + off-thread: the survey walks the card and
/// the library (to tell which sessions are safe to clear), so keep it off the UI
/// thread too.
#[tauri::command]
pub async fn scan_card() -> Option<CardInfo> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::scan_card(&cfg)).await {
        Ok(c) => c,
        Err(join) => {
            // Same trap as list_trips: `.ok().flatten()` made a panicked scan
            // look exactly like "no card inserted".
            reel_core::log::error("tauri", &format!("scan_card failed: {join}"));
            None
        }
    }
}

/// A clip's poster frame as a `data:` URI, generated and cached on first request.
/// `None` (placeholder in the UI) if the frame can't be made.
///
/// Async + off-thread: on a cache miss this shells out to ffmpeg to decode a frame
/// — from the master itself when a clip has no proxy, which on card media is a
/// multi-GB read. Run synchronously it blocked the UI thread, which also defeated
/// the frontend's `THUMB_MAX` throttle (the requests couldn't overlap, they just
/// queued on the main thread and froze the window on every cold open).
#[tauri::command]
pub async fn thumb(path: String, fileid: String) -> Option<String> {
    let cfg = Config::from_env();
    tauri::async_runtime::spawn_blocking(move || {
        reel_core::thumbs::poster_data_uri(&cfg, Path::new(&path), &fileid)
    })
    .await
    .ok()
    .flatten()
}

/// Copy the card's capture session spanning `[start, end]` into `trip`, streaming
/// per-chunk progress over `channel`. Runs the blocking copy off the UI thread so
/// the window stays live; resolves with a summary, or an error string on failure.
#[tauri::command]
pub async fn import_session(
    channel: Channel<ImportProgress>,
    trip: String,
    start: i64,
    end: i64,
) -> Result<ImportResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::import_window(&cfg, &trip, start, end, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("import task failed: {join}")),
    }
}

/// Push your masters in `trip` up to the shared cloud, verify them, and mark the
/// trip shared. Streams upload/verify progress over `channel`; runs off the UI
/// thread so the window stays live. Resolves with a summary, or an error string
/// (in which case the trip is left unshared and local copies are untouched).
#[tauri::command]
pub async fn share_trip(
    channel: Channel<PushProgress>,
    trip: String,
) -> Result<PushResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::push_trip(&cfg, &trip, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("share task failed: {join}")),
    }
}

/// Plan a card reclaim: match the card's masters (optionally scoped to one
/// session window) to verified local copies and confirm them in the cloud, without
/// deleting anything. Streams matching/verify progress over `channel`; resolves
/// with the exact set of files that are safe to delete, or an error (e.g. a trip
/// not fully in the cloud) that leaves the card untouched.
#[tauri::command]
pub async fn plan_reclaim(
    channel: Channel<WipeProgress>,
    start: Option<i64>,
    end: Option<i64>,
    offline: bool,
) -> Result<ReclaimPlan, String> {
    let cfg = Config::from_env();
    let window = start.zip(end);
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::plan_reclaim(&cfg, window, offline, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("reclaim plan failed: {join}")),
    }
}

/// Commit a previously-planned reclaim: delete exactly the files the plan
/// returned. Each is re-checked to live on the card before removal, so this can
/// never delete anything that isn't card footage.
#[tauri::command]
pub async fn commit_reclaim(files: Vec<String>) -> Result<ReclaimResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::commit_reclaim(&cfg, &files))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("reclaim commit failed: {join}")),
    }
}

/// Plan a trip archive: confirm every master is in the cloud and report what
/// freeing the local raw would reclaim, without deleting anything. Streams the
/// cloud check over `channel`.
#[tauri::command]
pub async fn plan_archive(
    channel: Channel<ArchiveProgress>,
    trip: String,
) -> Result<ArchivePlan, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::plan_archive(&cfg, &trip, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("archive plan failed: {join}")),
    }
}

/// Commit a trip archive: re-verify against the cloud (these are the only local
/// copies), then free the raw, keeping the cut clips and marks.
#[tauri::command]
pub async fn commit_archive(
    channel: Channel<ArchiveProgress>,
    trip: String,
) -> Result<ArchiveResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::commit_archive(&cfg, &trip, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("archive commit failed: {join}")),
    }
}

/// A trip's review playlist: clips in capture order (each with the proxy or
/// master to play) plus the marks saved so far. `Err` if the trip has no footage.
///
/// Async + off-thread: it walks the trip and loads the ledger, and Organize opens
/// by firing one of these per trip at once — synchronously they serialized on the
/// UI thread and froze it for the sum of every walk.
#[tauri::command]
pub async fn review_playlist(trip: String) -> Result<Playlist, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::review_playlist(&cfg, &trip))
        .await
    {
        Ok(r) => r,
        Err(join) => Err(format!("couldn't open the trip: {join}")),
    }
}

/// Replace a trip's `marks.tsv` with `marks` (the UI owns the whole list).
/// Returns the count written. The file stays compatible with `reel cut`.
#[tauri::command]
pub async fn save_marks(trip: String, marks: Vec<Mark>) -> Result<usize, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::save_marks(&cfg, &trip, marks))
        .await
    {
        Ok(r) => r,
        Err(join) => Err(format!("couldn't save marks: {join}")),
    }
}

/// A read-only preview playlist for the inserted card, optionally limited to one
/// capture session's `[start, end]` window. No marks — card footage isn't a trip
/// yet. Off the UI thread since it walks the card. `Err` if no card / no footage.
#[tauri::command]
pub async fn card_playlist(start: Option<i64>, end: Option<i64>) -> Result<Playlist, String> {
    let cfg = Config::from_env();
    let window = start.zip(end);
    match tauri::async_runtime::spawn_blocking(move || reel_core::card_playlist(&cfg, window)).await
    {
        Ok(result) => result,
        Err(join) => Err(format!("card scan failed: {join}")),
    }
}

/// Build (or find) a clean, webview-playable proxy for one card clip and return
/// its absolute path — the card-preview twin of `make_proxy` (no trip; cached by
/// content id under the cache dir). Runs ffmpeg off the UI thread.
#[tauri::command]
pub async fn make_card_proxy(master: String) -> Result<String, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::ensure_card_proxy(&cfg, Path::new(&master))
    })
    .await
    {
        Ok(result) => result.map(|p| p.display().to_string()),
        Err(join) => Err(format!("proxy task failed: {join}")),
    }
}

/// Build (or find) a clean, webview-playable proxy for one master and return its
/// absolute path. Remuxes a native `.LRF`/`.LRV` when present (fast) or transcodes
/// the master. Runs ffmpeg off the UI thread; a transcode can take a while.
#[tauri::command]
pub async fn make_proxy(trip: String, master: String) -> Result<String, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::ensure_proxy(&cfg, &trip, Path::new(&master))
    })
    .await
    {
        Ok(result) => result.map(|p| p.display().to_string()),
        Err(join) => Err(format!("proxy task failed: {join}")),
    }
}

/// Cut a trip's marked ranges into `clips/` — one lossless ffmpeg stream-copy per
/// mark, streaming per-segment progress over `channel`. Runs off the UI thread so
/// the window stays live; an output that already exists is left as-is, so this is
/// safe to re-run after adding more marks. `Err` only on a bad trip or no marks.
#[tauri::command]
pub async fn cut_trip(channel: Channel<CutProgress>, trip: String) -> Result<CutResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::cut_trip(&cfg, &trip, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("cut task failed: {join}")),
    }
}

/// Hand a trip's finished cut to the editor (Kdenlive), launched detached so the
/// GUI never owns the editor's lifetime. Falls back to the masters for a trip
/// that hasn't been cut yet. Runs off the UI thread; resolves with the number of
/// files opened, or an error string (bad/empty trip, or the editor isn't
/// installed).
#[tauri::command]
pub async fn open_in_editor(trip: String) -> Result<usize, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::open_in_editor(&cfg, &trip)).await
    {
        Ok(result) => result,
        Err(join) => Err(format!("edit task failed: {join}")),
    }
}

/// Probe a clip's playability (one ffprobe over headers) so the UI can flag an
/// empty, too-brief, or corrupt clip up front rather than waiting out a doomed
/// load. Runs off the UI thread; on any task failure it reports healthy so a
/// probe hiccup never blocks playback.
#[tauri::command]
pub async fn clip_health(path: String) -> reel_core::ClipHealth {
    let probed = path.clone();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::media::clip_health(Path::new(&probed))
    })
    .await
    {
        Ok(h) => h,
        Err(join) => {
            // Falling back to `ok: true` is right — an unprobed clip should still
            // get its chance to play — but doing it silently hid every ffprobe
            // crash behind a clip that then just refuses to load.
            reel_core::log::event(
                reel_core::log::Level::Warn,
                "tauri",
                &format!("clip_health failed, letting the clip try anyway: {join}"),
                Some(serde_json::json!({ "path": path })),
            );
            reel_core::ClipHealth {
                ok: true,
                tag: String::new(),
                reason: String::new(),
                duration: 0.0,
            }
        }
    }
}

/// Relocate one or more masters (absolute library paths) into `dest`, keeping the
/// ledger, marks, cut clips, and cloud in step — the fix for footage imported into
/// the wrong trip. Runs off the UI thread (a cloud sync may hit rclone).
#[tauri::command]
pub async fn move_clips(masters: Vec<String>, dest: String) -> Result<MoveResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::move_clips(&cfg, &masters, &dest))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("move task failed: {join}")),
    }
}

/// Rename a trip (dir + ledger + marks + cloud folder). `Err` if `new` already
/// exists (that's a merge) or either name is invalid.
#[tauri::command]
pub async fn rename_trip(old: String, new: String) -> Result<MoveResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::rename_trip(&cfg, &old, &new))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("rename task failed: {join}")),
    }
}

/// Fold every clip of `src` into `dst`, then remove the emptied source.
#[tauri::command]
pub async fn merge_trips(src: String, dst: String) -> Result<MoveResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::merge_trips(&cfg, &src, &dst))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("merge task failed: {join}")),
    }
}

/// Permanently delete masters (absolute library paths): erased locally, your own
/// erased from the cloud too, all tombstoned so they're never re-imported. Runs off
/// the UI thread (cloud deletes hit rclone). Irreversible — the UI confirms first.
#[tauri::command]
pub async fn delete_clips(masters: Vec<String>) -> Result<DeleteResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::delete_clips(&cfg, &masters))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("delete task failed: {join}")),
    }
}

/// Permanently delete a whole trip: local dir removed, clips tombstoned, and your
/// cloud contribution purged (other people's cloud footage kept). Irreversible.
#[tauri::command]
pub async fn delete_trip(trip: String) -> Result<DeleteResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::delete_trip(&cfg, &trip)).await {
        Ok(result) => result,
        Err(join) => Err(format!("delete task failed: {join}")),
    }
}

/// Delete the tombstoned files still on the card (trash you already permanently
/// deleted), optionally scoped to one session window. No cloud check — you already
/// decided these are gone. Guarded to card paths.
#[tauri::command]
pub async fn clear_discarded(
    start: Option<i64>,
    end: Option<i64>,
) -> Result<ReclaimResult, String> {
    let cfg = Config::from_env();
    let window = start.zip(end);
    match tauri::async_runtime::spawn_blocking(move || reel_core::clear_discarded(&cfg, window))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("clear-trash task failed: {join}")),
    }
}

/// Who has footage in this trip's cloud that you could pull down (excludes you).
/// Runs off the UI thread (one rclone listing).
#[tauri::command]
pub async fn cloud_contributors(trip: String) -> Result<Vec<Contributor>, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::cloud_contributors(&cfg, &trip))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("contributors task failed: {join}")),
    }
}

/// Pull one person's footage from the cloud into `trip`, streaming byte progress
/// over `channel`. Lands under `<trip>/<person>/`, so it reads as pulled-from-them
/// afterward. Runs off the UI thread.
#[tauri::command]
pub async fn pull_person(
    channel: Channel<PullProgress>,
    trip: String,
    person: String,
) -> Result<PullResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::pull_person(&cfg, &trip, &person, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("pull task failed: {join}")),
    }
}

/// A trip's sync status vs the shared cloud. `refresh` fetches the cloud live (and
/// updates the baseline + cache); otherwise it reads the cached listing. Runs off
/// the UI thread since a refresh hits rclone.
#[tauri::command]
pub async fn sync_status(trip: String, refresh: bool) -> Result<TripSync, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::sync_status(&cfg, &trip, refresh))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("sync status failed: {join}")),
    }
}

/// Reconcile one trip: replay owed cloud ops, then apply the chosen push / pull /
/// remove-from-cloud actions, streaming progress over `channel`. Runs off the UI
/// thread. Resolves with a summary of what actually ran.
#[tauri::command]
pub async fn sync_trip(
    channel: Channel<SyncProgress>,
    trip: String,
    actions: SyncActions,
) -> Result<SyncResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::reconcile(&cfg, &trip, actions, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("sync task failed: {join}")),
    }
}

/// Whether per-trip Nextcloud sharing is usable with the current cloud (a
/// Nextcloud webdav remote + curl). `Err(reason)` lets the UI disable the Sharing
/// panel with an explanation instead of failing mid-action. Off the UI thread —
/// it shells out to rclone.
#[tauri::command]
pub async fn sharing_status() -> Result<(), String> {
    let cfg = Config::from_env();
    tauri::async_runtime::spawn_blocking(move || reel_core::sharing_available(&cfg))
        .await
        .unwrap_or_else(|join| Err(format!("sharing check failed: {join}")))
}

/// The Nextcloud users a trip's cloud folder is currently shared with. Hits the
/// OCS Share API (reusing the rclone remote's credentials), so it runs off the UI
/// thread.
#[tauri::command]
pub async fn trip_shares(trip: String) -> Result<Vec<TripShare>, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::list_shares(&cfg, &trip)).await {
        Ok(result) => result,
        Err(join) => Err(format!("shares task failed: {join}")),
    }
}

/// Share a trip's cloud folder with a Nextcloud user (collaborator access, so they
/// can pull it and push their own footage). Resolves with the new share record.
#[tauri::command]
pub async fn share_add(trip: String, user: String) -> Result<TripShare, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::add_share(&cfg, &trip, &user))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("share task failed: {join}")),
    }
}

/// Revoke one share by its OCS id — the friend loses access to that trip's cloud
/// folder (their already-downloaded local copies are untouched).
#[tauri::command]
pub async fn share_remove(trip: String, id: String) -> Result<(), String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::remove_share(&cfg, &trip, &id))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("unshare task failed: {join}")),
    }
}

/// The people you've shared any trip with — the union of the local share caches,
/// to seed the add-a-friend dropdown. Network-free (reads cache files); async only
/// for consistency with the other share commands.
#[tauri::command]
pub async fn share_friends() -> Result<Vec<Sharee>, String> {
    let cfg = Config::from_env();
    Ok(reel_core::known_sharees(&cfg))
}

/// Autocomplete Nextcloud users for the "add friend" box.
#[tauri::command]
pub async fn sharee_search(query: String) -> Result<Vec<Sharee>, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::search_sharees(&cfg, &query))
        .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("sharee search failed: {join}")),
    }
}

/// Replay every owed cloud op across all trips (a global "Sync") — the way a
/// whole-trip purge owed from an offline delete gets applied, since its local
/// trip is gone. Streams replay progress over `channel`.
#[tauri::command]
pub async fn sync_all(channel: Channel<SyncProgress>) -> Result<SyncResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::reconcile_all(&cfg, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("sync-all task failed: {join}")),
    }
}

/// Scan the whole library and cloud for duplicate clips — the same clip living in
/// more than one trip, or a cloud orphan left by a reorg. Off the UI thread (the
/// cloud leg shells out to rclone).
#[tauri::command]
pub async fn dedup_scan() -> Result<DupReport, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || reel_core::scan_dupes(&cfg)).await {
        Ok(result) => result,
        Err(join) => Err(format!("duplicate scan failed: {join}")),
    }
}

/// Prune the chosen duplicate copies, keeping one canonical each and streaming
/// per-copy progress. Off the UI thread.
#[tauri::command]
pub async fn dedup_resolve(
    channel: Channel<DupProgress>,
    resolutions: Vec<DupResolution>,
) -> Result<DupResolveResult, String> {
    let cfg = Config::from_env();
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::resolve_dupes(&cfg, resolutions, |p| {
            let _ = channel.send(p);
        })
    })
    .await
    {
        Ok(result) => result,
        Err(join) => Err(format!("duplicate prune failed: {join}")),
    }
}

/// Write a line to the app log on the UI's behalf, so a JS error and the engine
/// fault that caused it land in the same file, in order. Deliberately infallible
/// and fire-and-forget: the UI must never have to handle a logging failure.
///
/// Synchronous on purpose — an append under a mutex costs microseconds, and going
/// through `spawn_blocking` would let lines land out of order.
#[tauri::command]
pub fn log_event(level: String, msg: String, ctx: Option<serde_json::Value>) {
    reel_core::log::event(reel_core::log::Level::parse(&level), "ui", &msg, ctx);
}

/// Where the log lives, for the UI to show when something has gone wrong.
#[tauri::command]
pub fn log_path() -> Option<String> {
    reel_core::log::path().map(|p| p.display().to_string())
}
