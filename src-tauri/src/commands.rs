//! Tauri commands — a thin, serializing layer over reel-core. Each builds a
//! fresh Config from the environment so the GUI honours the same env knobs.

use reel_core::{
    ArchivePlan, ArchiveProgress, ArchiveResult, CardInfo, Config, CutProgress, CutResult,
    ImportProgress, ImportResult, Mark, Playlist, PushProgress, PushResult, ReclaimPlan,
    ReclaimResult, Trip, WipeProgress,
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

#[tauri::command]
pub fn list_trips() -> Vec<Trip> {
    reel_core::list_trips(&Config::from_env())
}

#[tauri::command]
pub fn scan_card() -> Option<CardInfo> {
    reel_core::scan_card(&Config::from_env())
}

/// A clip's poster frame as a `data:` URI, generated and cached on first request.
/// `None` (placeholder in the UI) if the frame can't be made.
#[tauri::command]
pub fn thumb(path: String, fileid: String) -> Option<String> {
    reel_core::thumbs::poster_data_uri(&Config::from_env(), Path::new(&path), &fileid)
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

/// Push your masters in `trip` up to the shared pool, verify them, and mark the
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
/// session window) to verified local copies and confirm them in the pool, without
/// deleting anything. Streams matching/verify progress over `channel`; resolves
/// with the exact set of files that are safe to delete, or an error (e.g. a trip
/// not fully in the pool) that leaves the card untouched.
#[tauri::command]
pub async fn plan_reclaim(
    channel: Channel<WipeProgress>,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<ReclaimPlan, String> {
    let cfg = Config::from_env();
    let window = start.zip(end);
    match tauri::async_runtime::spawn_blocking(move || {
        reel_core::plan_reclaim(&cfg, window, false, |p| {
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

/// Plan a trip archive: confirm every master is in the pool and report what
/// freeing the local raw would reclaim, without deleting anything. Streams the
/// pool check over `channel`.
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

/// Commit a trip archive: re-verify against the pool (these are the only local
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
#[tauri::command]
pub fn review_playlist(trip: String) -> Result<Playlist, String> {
    reel_core::review_playlist(&Config::from_env(), &trip)
}

/// Replace a trip's `marks.tsv` with `marks` (the UI owns the whole list).
/// Returns the count written. The file stays compatible with `reel cut`.
#[tauri::command]
pub fn save_marks(trip: String, marks: Vec<Mark>) -> Result<usize, String> {
    reel_core::save_marks(&Config::from_env(), &trip, marks)
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

/// Probe a clip's playability (one ffprobe over headers) so the UI can flag an
/// empty, too-brief, or corrupt clip up front rather than waiting out a doomed
/// load. Runs off the UI thread; on any task failure it reports healthy so a
/// probe hiccup never blocks playback.
#[tauri::command]
pub async fn clip_health(path: String) -> reel_core::ClipHealth {
    tauri::async_runtime::spawn_blocking(move || reel_core::media::clip_health(Path::new(&path)))
        .await
        .unwrap_or(reel_core::ClipHealth {
            ok: true,
            tag: String::new(),
            reason: String::new(),
            duration: 0.0,
        })
}
