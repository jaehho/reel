//! Serializable types handed to the UI.

use serde::{Deserialize, Serialize};

/// Where a trip sits in the pipeline — drives the dashboard's next-step hint.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum TripState {
    /// No local footage and nothing cut yet.
    Empty,
    /// Footage imported, not reviewed.
    Imported,
    /// Has marks but no cut clips yet.
    Marked,
    /// Has cut clips, ready to edit.
    Cut,
    /// Raw freed locally (kept in pool); clips remain.
    Archived,
}

impl TripState {
    /// One-word next action the UI turns into a button.
    pub fn next(self) -> &'static str {
        match self {
            TripState::Empty => "import",
            TripState::Imported => "review",
            TripState::Marked => "cut",
            TripState::Cut => "edit",
            TripState::Archived => "edit",
        }
    }
}

/// Whether *your* masters in a trip are up in the shared pool (everyone's raw
/// footage on the remote — not a personal backup). Read from `.reel`; `Unknown`
/// until a verified push records it, so the UI never implies safety it can't
/// confirm. Footage pulled from others is in the pool by definition.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Share {
    Shared,
    Local,
    Unknown,
}

/// A pointer to one clip: path plus the stable content id, so the UI can request
/// that clip's poster without re-hashing it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ClipRef {
    pub path: String,
    pub fileid: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Trip {
    pub name: String,
    pub path: String,
    pub masters: usize,
    pub marks: usize,
    pub clips: usize,
    pub bytes: u64,
    /// Capture window from `.reel` (`YYYY-MM-DD`), if set.
    pub from: Option<String>,
    pub to: Option<String>,
    /// Capture range derived from the masters' timestamps (epoch seconds).
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub state: TripState,
    pub next: String,
    /// Representative clip for the card's cover image (first master), if any.
    pub cover: Option<ClipRef>,
    /// Share state of *your* masters (footage you pulled is in the pool already).
    pub share: Share,
    /// Masters you shot (under `<trip>/<you>/`) vs. footage pulled from others.
    /// `mine + pulled == masters`.
    pub mine: usize,
    pub pulled: usize,
    /// Other people whose footage you've pulled into this trip, sorted.
    pub contributors: Vec<String>,
}

/// One capture session detected on a card (clips clustered by time gap).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub index: usize,
    pub start: i64,
    pub end: i64,
    pub clips: usize,
    pub bytes: u64,
    /// Trips that already own one or more of this session's clips.
    pub owners: Vec<String>,
    /// Every clip already imported somewhere.
    pub imported: bool,
    /// Clips not yet imported anywhere.
    pub new_clips: usize,
    /// A few frames spread across the session, for a contact strip.
    pub strip: Vec<ClipRef>,
    /// Imported into a trip that's verified in the pool → safe to clear the card.
    pub safe: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CardInfo {
    pub roots: Vec<String>,
    pub clips: usize,
    pub bytes: u64,
    pub sessions: Vec<Session>,
}

/// Streamed during an import — one per chunk copied, so the UI can show a live
/// byte-accurate bar plus the file currently in flight.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    /// Basename of the clip currently copying.
    pub file: String,
    /// 1-based position of this clip in the batch, and the batch size.
    pub file_index: usize,
    pub file_count: usize,
    /// Bytes copied of the current clip, and its size.
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Bytes copied across the whole batch, and the batch total.
    pub copied_bytes: u64,
    pub total_bytes: u64,
}

/// Summary handed back when an import finishes.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub trip: String,
    /// Clips newly copied in.
    pub copied: usize,
    /// Bytes newly copied in.
    pub bytes: u64,
    /// Clips already present in this trip (resume/backfill), skipped.
    pub skipped_here: usize,
    /// Clips owned by a different trip, left untouched.
    pub skipped_other: usize,
}

/// Which leg of a share push is running, so the UI can label the bar. Upload
/// reports bytes; verify reports files checked.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum PushPhase {
    Upload,
    Verify,
}

/// Streamed while pushing a trip to the shared pool — figures come straight from
/// rclone's JSON stats, so the bar tracks the real transfer.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PushProgress {
    pub phase: PushPhase,
    /// A file currently in flight (basename), when rclone reports one.
    pub file: String,
    /// Upload: bytes done / total. Verify: files checked / total.
    pub done: u64,
    pub total: u64,
}

/// Summary handed back once a push is verified and the trip is marked shared.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    pub trip: String,
    /// Your masters now confirmed in the pool.
    pub files: usize,
    /// Their total size.
    pub bytes: u64,
    /// Bytes actually sent this run (0 → everything was already up).
    pub uploaded: u64,
}

/// Which leg of a card reclaim is running. Match hashes card files against the
/// ledger; verify confirms each master is in the pool before anything is deleted.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum WipePhase {
    Match,
    Verify,
}

/// Streamed while planning a reclaim (matching + pool check); no deletion happens
/// during this.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WipeProgress {
    pub phase: WipePhase,
    pub done: u64,
    pub total: u64,
    /// The file being matched, or the trip being verified.
    pub label: String,
}

/// What a reclaim *would* delete, once card files are matched to verified local
/// copies and those masters are confirmed in the pool. Nothing is deleted to
/// produce this — the user confirms it, then `commit_reclaim` removes `files`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimPlan {
    /// Absolute card paths cleared for deletion, handed back to commit verbatim.
    pub files: Vec<String>,
    /// Their total size on the card.
    pub bytes: u64,
    /// Trips these masters belong to, confirmed present in the pool.
    pub trips: Vec<String>,
    /// Card masters with no import record — left on the card.
    pub not_imported: usize,
    /// Imported, but the local copy is missing or a different size — left alone.
    pub not_verified: usize,
    /// The pool check was skipped (offline reclaim).
    pub offline: bool,
}

/// Outcome of committing a reclaim: what actually left the card.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimResult {
    pub deleted: usize,
    pub bytes: u64,
}

/// Streamed while archiving a trip — rclone's per-file count as it confirms every
/// master is in the pool before any local raw is freed.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveProgress {
    pub done: u64,
    pub total: u64,
}

/// What archiving a trip *would* free: its raw masters are confirmed in the pool,
/// and freeing the local copies would reclaim `bytes`. No deletion happens to
/// produce this.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArchivePlan {
    pub trip: String,
    /// Raw masters confirmed in the pool (every one, all contributors).
    pub masters: usize,
    /// Local bytes that freeing would reclaim — the per-person raw trees plus
    /// proxies/sheets; the cut clips are kept.
    pub bytes: u64,
}

/// Outcome of archiving: the local raw that was freed (clips/marks stay).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveResult {
    pub trip: String,
    pub freed: u64,
    pub masters: usize,
}

/// Streamed while cutting a trip — one per marked segment as ffmpeg starts on it,
/// so the UI can name the clip being written and tick a bar by segment count.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CutProgress {
    /// Basename of the output clip being written.
    pub file: String,
    /// 1-based position in the mark list, and the number of marks.
    pub index: usize,
    pub count: usize,
}

/// Summary of a cut: clips newly written, marks skipped because their output was
/// already there (the cut is additive and re-runnable), marks that failed, and
/// where the clips landed.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CutResult {
    pub trip: String,
    pub made: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Absolute path of the trip's `clips/` directory.
    pub dir: String,
}

/// One marked segment of a master, in master-timeline seconds. Round-trips
/// through `marks.tsv` byte-compatibly with the script (`master\tstart\tend\t
/// label`), so `reel cut` consumes GUI marks unchanged. Carries both ways: sent
/// to the UI in a playlist, and received back to rewrite the file.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    /// Absolute path of the master this segment cuts from — the TSV key.
    pub master: String,
    pub start: f64,
    pub end: f64,
    pub label: String,
}

/// One clip in a review playlist: the master (what marks key on and `cut` reads)
/// and the file to load in the webview. Native `.LRF`/`.LRV` proxies carry extra
/// video/data streams the `<video>` element chokes on, so they're never played
/// raw — instead a clean single-stream mp4 is remuxed from them on demand and
/// cached under `.proxies/`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReviewClip {
    /// Absolute master path; marks record this regardless of what played.
    pub master: String,
    /// Absolute path to load now: a cached clean proxy if one exists, else the
    /// master. When `proxied` is false the UI may still need to build one first.
    pub play: String,
    /// Master basename, for display.
    pub name: String,
    /// Content id, so the player can request the same cached poster as the grid.
    pub fileid: String,
    /// Capture time (epoch seconds), for the clip's caption.
    pub captured: i64,
    /// Master size in bytes.
    pub bytes: u64,
    /// True when `play` is a ready cached proxy — load it directly.
    pub proxied: bool,
    /// A native `.LRF`/`.LRV` sits beside the master, so building a clean proxy is
    /// a fast remux (no re-encode). The UI builds eagerly in that case rather than
    /// streaming a multi-GB master.
    pub has_proxy: bool,
    /// The master is a card stub — a placeholder the camera wrote with no actual
    /// video (a couple hundred bytes). It can never play; the UI skips and marks it.
    pub stub: bool,
}

/// A quick ffprobe verdict on whether a clip is worth loading. Lets the player
/// flag empty/blip/corrupt clips up front instead of waiting out a doomed load.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ClipHealth {
    /// Safe to try playing.
    pub ok: bool,
    /// Short filmstrip tag when not ok: `empty` | `brief` | `unreadable`.
    pub tag: String,
    /// A full sentence for the player overlay.
    pub reason: String,
    /// Duration in seconds, best-effort (0 if unknown).
    pub duration: f64,
}

/// Everything the review view needs for a trip: its clips in capture order and
/// the marks already saved (across all clips, in file order).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub trip: String,
    pub clips: Vec<ReviewClip>,
    pub marks: Vec<Mark>,
}
