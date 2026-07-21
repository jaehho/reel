//! Serializable types handed to the UI.

use serde::{Deserialize, Serialize};

/// How far a trip has got — drives the dashboard's next-step hint.
///
/// Reviewing is the only step that changes what a trip *is*: before it, the
/// footage is undifferentiated; after it, the parts worth keeping are named. So
/// the states track review, and stop there. Cutting used to sit at the end of
/// this list, back when clips were the only way into an editor — but a trip's
/// marks now open as a Kdenlive timeline directly, so cutting is one of two
/// things you can do with a finished trip rather than a step on the way to
/// them. It's an export, and an export doesn't advance anything: the clip count
/// is on the card either way, and `Marked` stays true whether you've run it once,
/// five times, or never.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum TripState {
    /// No local footage.
    Empty,
    /// Footage imported, not reviewed.
    Imported,
    /// Reviewed: has marks, ready to edit or cut.
    Marked,
    /// Raw freed locally, kept in the cloud.
    Archived,
}

impl TripState {
    /// One-word next action the UI turns into a button.
    pub fn next(self) -> &'static str {
        match self {
            TripState::Empty => "import",
            TripState::Imported => "review",
            TripState::Marked => "edit",
            // Nothing here to work on until the raw is back, and every other
            // action on an archived trip is a lie (see `footActions`).
            TripState::Archived => "restore",
        }
    }
}

/// Whether *your* masters in a trip are up in the shared cloud (everyone's raw
/// footage on the remote — not a personal backup). Read from `.reel`; `Unknown`
/// until a verified push records it, so the UI never implies safety it can't
/// confirm. Footage pulled from others is in the cloud by definition.
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
    /// Share state of *your* masters (footage you pulled is in the cloud already).
    pub share: Share,
    /// Masters you shot (under `<trip>/<you>/`) vs. footage pulled from others.
    /// `mine + pulled == masters`.
    pub mine: usize,
    pub pulled: usize,
    /// Other people whose footage you've pulled into this trip, sorted.
    pub contributors: Vec<String>,
    /// Live sync status for the card chip — computed from the baseline plus the
    /// cached cloud listing, so it rides along with `list_trips` without a network
    /// hit. The Sync panel fetches the cloud fresh for the full picture.
    pub sync: SyncBrief,
    /// Nextcloud users this trip's cloud folder is shared with, from the last
    /// Sharing fetch (network-free from a cache). `None` = never checked / not in
    /// the cloud → no chip; `Some(0)` = in the cloud, shared with nobody yet → a
    /// "Share…" quick-action chip; `Some(n)` = shared with n people.
    pub shared_with: Option<usize>,
}

/// Compact per-trip sync counts embedded in `Trip` so the dashboard chip is live
/// and network-free. Mirrors the buckets of `TripSync`. `lastPoolCheck` is `None`
/// until the cloud has been fetched at least once for this trip.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncBrief {
    /// Your masters not yet in the cloud — the "to share" count.
    pub to_push: usize,
    /// Footage in the cloud you don't have locally yet (from the cache).
    pub to_pull: usize,
    /// Your clips removed locally but still in the cloud — owed cleanup.
    pub deleted_local: usize,
    /// Footage removed from the cloud that you'd pulled — orphaned local copies.
    pub deleted_upstream: usize,
    /// In the cloud but not on this machine, and not something you deleted — your
    /// archived (raw freed) footage, and pulled clips you cleared locally. Safe,
    /// re-downloadable; informational, doesn't count as drift.
    pub cloud_only: usize,
    /// Present both sides at different sizes.
    pub conflicts: usize,
    /// Owed cloud ops (move/rename/purge) waiting on the remote coming back.
    pub pending: usize,
    /// When the cloud was last fetched for this trip (epoch seconds), if ever.
    pub last_cloud_check: Option<i64>,
    /// Nothing to reconcile: no push/pull/cleanup/conflict outstanding.
    pub in_sync: bool,
}

/// One clip in a sync diff, for the panel's rows.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SyncItem {
    /// Cloud-relative path `person/camera/base`.
    pub rel: String,
    /// Owning person (the first segment of `rel`).
    pub person: String,
    /// Basename, for display.
    pub name: String,
    pub bytes: u64,
    /// True when the clip is under your own folder.
    pub mine: bool,
}

/// The full sync picture for one trip: every drift bucket with its clips, so the
/// Sync panel can list and act on each. `offline` means a fresh cloud fetch was
/// asked for but the remote was unreachable (buckets then reflect the cache).
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TripSync {
    pub trip: String,
    /// Yours, present locally, missing from the cloud → upload.
    pub to_push: Vec<SyncItem>,
    /// In the cloud, not local → download (grouped by person in the UI).
    pub to_pull: Vec<SyncItem>,
    /// Yours, removed locally, still in the cloud → offer to remove from cloud.
    pub deleted_local: Vec<SyncItem>,
    /// Removed from the cloud, still local (someone else's) → orphan notice.
    pub deleted_upstream: Vec<SyncItem>,
    /// In the cloud but not on this machine (and not deleted by you) — archived raw
    /// you freed locally, or pulled clips you cleared. Re-downloadable; not drift.
    pub cloud_only: Vec<SyncItem>,
    /// Present both sides, different size → surface (never auto-resolved).
    pub conflicts: Vec<SyncItem>,
    /// Owed cloud ops replayed on the next reconcile.
    pub pending: usize,
    pub last_cloud_check: Option<i64>,
    /// A refresh was requested but the cloud was unreachable.
    pub offline: bool,
    pub in_sync: bool,
}

/// Which reconcile actions the user chose in the panel. Deletions are their own
/// opt-in so Sync never removes cloud footage without an explicit tick.
#[derive(Deserialize, Clone, Copy, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncActions {
    /// Upload your local-new footage (and re-verify), marking the trip shared.
    pub push: bool,
    /// Pull footage others added to the cloud.
    pub pull: bool,
    /// Remove your `deleted_local` clips from the cloud.
    pub push_deletions: bool,
    /// Download the `cloud_only` clips back onto this machine — the way back from
    /// archiving your raw or clearing a pulled clip. Opt-in: you freed that disk on
    /// purpose, so nothing re-fills it unless you ask.
    pub restore_cloud: bool,
}

/// Which leg of a reconcile is running, so the UI can label the bar.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncPhase {
    /// Replaying owed move/rename/purge ops.
    Replay,
    /// Fetching the cloud listing.
    #[default]
    Check,
    Push,
    Pull,
    /// Bringing `cloud_only` footage back down.
    Restore,
    /// Removing owed cloud copies.
    Delete,
}

/// Streamed while reconciling — figures come from the underlying push/pull rclone
/// stats, or a simple item count for replay/delete legs.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub phase: SyncPhase,
    pub file: String,
    pub done: u64,
    pub total: u64,
    /// The trip this progress is about — set for every per-trip leg so a global
    /// sync can name the trip in flight. Empty during the global owed-op flush.
    pub trip: String,
    /// Position of this trip in a global sweep (1-based) and the sweep size. `0`/`0`
    /// during a single-trip reconcile or the pre-sweep owed-op flush, so the UI
    /// shows an overall "trip X of Y" only when it applies.
    pub trip_index: u32,
    pub trip_count: u32,
}

/// Summary handed back once a reconcile finishes.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub trip: String,
    /// Masters uploaded (or re-verified) this run.
    pub pushed: usize,
    /// Masters pulled down this run.
    pub pulled: usize,
    /// Masters brought back from the cloud this run (`cloud_only`).
    pub restored: usize,
    /// Cloud copies removed this run.
    pub deleted: usize,
    /// Owed ops successfully replayed.
    pub replayed: usize,
    /// Owed ops still queued (remote still couldn't take them).
    pub still_pending: usize,
    pub in_sync: bool,
}

/// One place a clip lives — a `(trip, cloud-relative path)` location that may be on
/// disk, in the cloud, or both. The same content at two different locations is what
/// the duplicate scan flags.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DupCopy {
    pub trip: String,
    /// Owning person (first segment of `rel`).
    pub person: String,
    /// Cloud-relative path within the trip (`person/camera/base` or `person/base`).
    pub rel: String,
    /// Basename, for display.
    pub name: String,
    pub bytes: u64,
    /// Present in the local library.
    pub local: bool,
    /// Present in the shared cloud.
    pub in_cloud: bool,
    /// Under your own folder (`REEL_USER`).
    pub mine: bool,
    /// Absolute local path when `local`, so the UI can preview it; `None` for a
    /// cloud-only copy.
    pub path: Option<String>,
}

/// A set of copies that are the same clip (same basename + byte size) living in
/// more than one place. `suggestedKeep` indexes the copy the scan would keep by
/// default (a fully-synced copy in a named trip); the rest are redundant.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DupGroup {
    /// Stable identity key (`base|size`), for the UI's list keys.
    pub key: String,
    /// The clip basename.
    pub name: String,
    /// One copy's byte size — the other half of the identity (all copies share it).
    pub bytes: u64,
    pub copies: Vec<DupCopy>,
    /// Bytes reclaimed by pruning down to one copy (`bytes * (copies - 1)`).
    pub reclaimable: u64,
    /// Index into `copies` of the default keep.
    pub suggested_keep: usize,
}

/// The result of a whole-library duplicate scan.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DupReport {
    pub groups: Vec<DupGroup>,
    /// Duplicate groups found.
    pub groups_count: usize,
    /// Total redundant bytes across all groups.
    pub total_reclaimable: u64,
    /// Local master files scanned.
    pub scanned_local: usize,
    /// Cloud master files scanned (0 when offline).
    pub scanned_cloud: usize,
    /// The cloud couldn't be listed — groups reflect local footage only, and
    /// cloud-side pruning is unavailable.
    pub offline: bool,
}

/// One clip location, sent back from the panel to keep or prune.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DupLoc {
    pub trip: String,
    pub rel: String,
    pub local: bool,
    pub in_cloud: bool,
    pub bytes: u64,
}

/// One group's resolution: keep `keep`, prune every copy in `remove`.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DupResolution {
    pub keep: DupLoc,
    pub remove: Vec<DupLoc>,
}

/// Streamed while pruning duplicates — one per copy as it's removed.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DupProgress {
    pub file: String,
    pub done: u64,
    pub total: u64,
}

/// Outcome of a prune. `cloudOk` is false when a cloud deletion was needed but the
/// remote was unreachable — local copies were still pruned, and the cloud cleanup is
/// owed (the next sync sees it as a zombie).
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DupResolveResult {
    /// Redundant local files unlinked.
    pub removed_local: usize,
    /// Redundant cloud copies deleted.
    pub removed_cloud: usize,
    /// Cloud copies left alone because they're someone else's contribution — the
    /// local duplicate is still pruned, but a friend's cloud copy is theirs to keep
    /// (same rule `remove::delete_clips` follows).
    pub kept_cloud: usize,
    /// Redundant bytes reclaimed.
    pub freed: u64,
    /// Groups acted on.
    pub groups: usize,
    /// Copies skipped for safety (canonical unverifiable, a content mismatch, or
    /// the canonical itself).
    pub skipped: usize,
    pub cloud_ok: bool,
    pub offline: bool,
}

/// One capture session detected on a card (captures clustered by time gap).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub index: usize,
    pub start: i64,
    pub end: i64,
    /// Every capture in the session — videos and photos together.
    pub captures: usize,
    /// How many of `captures` are photos (the rest are videos); for a friendly
    /// "9 clips · 3 photos" breakdown. Import/clear logic treats all alike.
    pub photos: usize,
    /// Total size of every capture in the session.
    pub bytes: u64,
    /// Trips that already own one or more of this session's captures.
    pub owners: Vec<String>,
    /// Every capture already imported somewhere.
    pub imported: bool,
    /// Captures not yet imported anywhere (excludes discarded ones).
    pub new_captures: usize,
    /// Captures the user permanently deleted but still sitting on the card — trash
    /// to clear, never re-imported.
    pub discarded: usize,
    /// A few frames spread across the session, for a contact strip.
    pub strip: Vec<ClipRef>,
    /// Imported into a trip that's verified in the cloud → safe to clear the card.
    pub safe: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CardInfo {
    pub roots: Vec<String>,
    /// Total captures across the card (videos + photos).
    pub captures: usize,
    pub bytes: u64,
    /// How many of `captures` are photos.
    pub photos: usize,
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
    /// Captures newly copied in (videos + photos).
    pub copied: usize,
    /// Bytes newly copied in.
    pub bytes: u64,
    /// Captures already present in this trip (resume/backfill), skipped.
    pub skipped_here: usize,
    /// Captures owned by a different trip, left untouched.
    pub skipped_other: usize,
    /// How many of `copied` are photos.
    pub photos: usize,
}

/// Outcome of relocating footage — one or more clips moved between trips, a trip
/// renamed, or one trip merged into another. `cloud_synced` is false when footage
/// that was in the shared cloud couldn't be moved there too (offline), so the
/// destination's share claim was dropped to `unknown` rather than left overstating
/// safety; the UI then nudges a re-Share.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MoveResult {
    /// The trip footage landed in.
    pub dest: String,
    /// Masters relocated.
    pub moved: usize,
    /// Cut clips (`clips/*`) carried along with their masters.
    pub clips: usize,
    /// Marks migrated to the destination's `marks.tsv`.
    pub marks: usize,
    /// Masters left alone because the destination already had them (dedup).
    pub skipped: usize,
    /// The cloud mirrored the move (or nothing in_cloud needed moving).
    pub cloud_synced: bool,
}

/// Outcome of a permanent delete — clips (or a whole trip) erased for good. Your
/// own footage is removed locally **and** from the cloud; footage pulled from
/// someone else is only removed locally (`kept_cloud`), since the cloud copy is
/// theirs. Every deleted clip is tombstoned so a copy still on a card reads
/// "discarded", never re-imported.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    /// Local masters removed.
    pub deleted: usize,
    /// Local bytes freed.
    pub bytes: u64,
    /// Your masters also erased from the cloud.
    pub in_cloud: usize,
    /// Masters left in the cloud because they're someone else's (pulled).
    pub kept_cloud: usize,
    /// The cloud removals ran (false → offline; erased locally, cloud cleanup owed).
    pub cloud_ok: bool,
}

/// Which leg of a share push is running, so the UI can label the bar. Upload
/// reports bytes; verify reports files checked.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum PushPhase {
    Upload,
    Verify,
}

/// Streamed while pushing a trip to the shared cloud — figures come straight from
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
    /// Upload throughput in bytes/sec (rclone's running average); 0 when unknown.
    pub speed: u64,
    /// Seconds remaining per rclone's estimate; -1 when unknown.
    pub eta: i64,
}

/// Summary handed back once a push is verified and the trip is marked shared.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    pub trip: String,
    /// Your masters now confirmed in the cloud.
    pub files: usize,
    /// Their total size.
    pub bytes: u64,
    /// Bytes actually sent this run (0 → everything was already up).
    pub uploaded: u64,
}

/// A person with footage in this trip's shared cloud that you could pull down.
/// Provenance is the folder: their clips live under `<trip>/<person>/` in the cloud
/// exactly as they will locally. Excludes you — your own footage is a Share, not a
/// pull.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Contributor {
    pub person: String,
    pub clips: usize,
    pub bytes: u64,
    /// You already have their footage locally (nothing new to pull).
    pub pulled: bool,
}

/// One Nextcloud user a trip's cloud folder is shared with, via the OCS Share
/// API. reel manages these so a friend sees only the trips they're on, not the
/// whole cloud. `id` is the OCS share id used to revoke it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TripShare {
    pub id: String,
    /// The Nextcloud user id the folder is shared with.
    pub user: String,
    /// Their display name if the server gave one, else the id.
    pub display_name: String,
    /// OCS permission bitmask (1 read | 2 update | 4 create | 8 delete | 16 reshare).
    pub permissions: u32,
}

/// A candidate share recipient from the Nextcloud Sharees autocomplete, for the
/// "add friend" box.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Sharee {
    pub user: String,
    pub display_name: String,
}

/// Streamed while pulling a person's footage down from the cloud — rclone's byte
/// stats, so the bar tracks the real download.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PullProgress {
    /// A file currently in flight (basename), when rclone reports one.
    pub file: String,
    pub done: u64,
    pub total: u64,
}

/// Summary handed back once a person's footage has been pulled into a trip.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PullResult {
    pub trip: String,
    pub person: String,
    /// Masters now present locally under `<trip>/<person>/`.
    pub files: usize,
    pub bytes: u64,
}

/// Summary handed back once `cloud_only` footage has been brought back down.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub trip: String,
    /// Masters that actually landed — counted on disk, not assumed from the ask.
    pub files: usize,
    pub bytes: u64,
}

/// Which leg of a card reclaim is running. Match hashes card files against the
/// ledger; verify confirms each master is in the cloud before anything is deleted.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum WipePhase {
    Match,
    Verify,
}

/// Streamed while planning a reclaim (matching + cloud check); no deletion happens
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
/// copies and those masters are confirmed in the cloud. Nothing is deleted to
/// produce this — the user confirms it, then `commit_reclaim` removes `files`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimPlan {
    /// Absolute card paths cleared for deletion, handed back to commit verbatim.
    pub files: Vec<String>,
    /// Their total size on the card.
    pub bytes: u64,
    /// Trips these masters belong to, confirmed present in the cloud.
    pub trips: Vec<String>,
    /// Card masters with no import record — left on the card.
    pub not_imported: usize,
    /// Imported, but the local copy is missing or a different size — left alone.
    pub not_verified: usize,
    /// The cloud check was skipped (offline reclaim).
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
/// master is in the cloud before any local raw is freed.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveProgress {
    pub done: u64,
    pub total: u64,
}

/// What archiving a trip *would* free: its raw masters are confirmed in the cloud,
/// and freeing the local copies would reclaim `bytes`. No deletion happens to
/// produce this.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArchivePlan {
    pub trip: String,
    /// Raw masters confirmed in the cloud (every one, all contributors).
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

/// A frame grabbed from a master and kept as a photo capture beside it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StillResult {
    /// Absolute path to the new JPEG.
    pub path: String,
    /// Its basename, for the toast and the filmstrip caption.
    pub name: String,
    /// Content id, so the UI can request its poster like any other capture.
    pub fileid: String,
    pub bytes: u64,
}

/// Where a trip's marks landed as a Kdenlive timeline. The counterpart to
/// `CutResult`: `cut` reports files written, this reports one project holding every
/// mark as a clip that can still be re-trimmed against its master.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TimelineResult {
    pub trip: String,
    /// Absolute path of the `.kdenlive` project.
    pub path: String,
    /// Marks placed on the timeline.
    pub segments: usize,
    /// Distinct masters the timeline references.
    pub sources: usize,
    /// Marks left out because their master is missing or unreadable — an archived
    /// trip's raw, or the odd file the camera never finished writing.
    pub skipped: usize,
    /// Timeline length in seconds.
    pub duration: f64,
    /// The project's video format, e.g. `3840×2160 · 23.98 fps`.
    pub profile: String,
    /// The stock MLT profile Kdenlive will open the project in (`uhd_2160p_2398`).
    /// `None` when the footage matches no stock profile — portrait drone video at
    /// 23.98 is the real case. Kdenlive then opens at *its* default rather than the
    /// footage's own format, which is worth saying out loud: nothing else would
    /// tell you, and the picture just quietly comes out the wrong shape.
    pub profile_id: Option<String>,
    /// Set when the project's frame rate was rounded because the footage's real rate
    /// has no standard Kdenlive profile — holds the footage's actual format, e.g.
    /// `1080×1920 · 23.98 fps`. Kdenlive refuses a fractional rate on non-standard
    /// geometry with a modal on every open, so the rate is conformed to the nearest
    /// integer (0.1%, under a frame across a 30 s timeline) and every frame position
    /// is computed against it. Worth surfacing: it's a real, if small, compromise.
    pub conformed_from: Option<String>,
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
    /// Small source to pull a poster / filmstrip frame from: a native
    /// `.LRF`/`.LRV` (or a built proxy) when present, else the master. Card
    /// masters run to many GB on slow media — grabbing a frame from the tiny
    /// proxy instead keeps the player and filmstrip responsive.
    pub poster: String,
    /// True when `play` is a ready cached proxy — load it directly.
    pub proxied: bool,
    /// A native `.LRF`/`.LRV` sits beside the master, so building a clean proxy is
    /// a fast remux (no re-encode). The UI builds eagerly in that case rather than
    /// streaming a multi-GB master.
    pub has_proxy: bool,
    /// The master is a card stub — a placeholder the camera wrote with no actual
    /// video (a couple hundred bytes). It can never play; the UI skips and marks it.
    pub stub: bool,
    /// A still photo, not a video: the UI shows it as an image (no proxy, no
    /// scrubbing, no marks). Stitched panoramas and ordinary pictures are photos.
    pub photo: bool,
    /// The person whose folder this clip sits in (`<trip>/<person>/…`) — its
    /// provenance, shown as a badge while reviewing.
    pub person: String,
    /// True when that person is you (`REEL_USER`); the badge reads "you".
    pub mine: bool,
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
