//! reel-core — the engine behind the reel GUI.
//!
//! Pure Rust, no GUI dependency, so it's unit-tested headlessly. Ports the
//! footage logic from the original `reel` script: card/session survey, the
//! content-id dedup ledger, trip discovery and state, and poster thumbnails.
//! Mutating phases (import, cut, cloud sync, wipe) land here too as they're built.
#![allow(dead_code)]

pub mod archive;
pub mod cards;
pub mod config;
pub mod cut;
pub mod dedup;
pub mod edit;
pub mod import;
pub mod ledger;
pub mod log;
pub mod media;
pub mod model;
pub mod organize;
pub mod proxy;
pub mod pull;
pub mod push;
pub mod rclone;
pub mod remove;
pub mod restore;
pub mod review;
pub mod serve;
pub mod sessions;
pub mod share;
pub mod store;
pub mod sync;
pub mod thumbs;
pub mod trips;
pub mod wipe;

pub use archive::{commit_archive, plan_archive};
pub use cards::scan_card;
pub use config::Config;
pub use cut::cut_trip;
pub use dedup::{resolve as resolve_dupes, scan as scan_dupes};
pub use edit::open_in_editor;
pub use import::import_window;
pub use model::{
    ArchivePlan, ArchiveProgress, ArchiveResult, CardInfo, ClipHealth, ClipRef, Contributor,
    CutProgress, CutResult, DeleteResult, DupCopy, DupGroup, DupLoc, DupProgress, DupReport,
    DupResolution, DupResolveResult, ImportProgress, ImportResult, Mark, MoveResult, Playlist,
    PullProgress, PullResult, PushPhase, PushProgress, PushResult, ReclaimPlan, ReclaimResult,
    RestoreResult, ReviewClip, Session, Share, Sharee, SyncActions, SyncBrief, SyncItem, SyncPhase,
    SyncProgress, SyncResult, Trip, TripShare, TripState, TripSync, WipePhase, WipeProgress,
};
pub use organize::{merge_trips, move_clips, rename_trip};
pub use proxy::{ensure_card_proxy, ensure_proxy};
pub use pull::{cloud_contributors, pull_person};
pub use push::push_trip;
pub use remove::{clear_discarded, delete_clips, delete_trip};
pub use restore::restore;
pub use review::{card_playlist, review_playlist, save_marks};
pub use share::{
    add_share, known_sharees, list_shares, remove_share, search_sharees, sharing_available,
};
pub use sync::{reconcile, reconcile_all, sync_status, trips_in_cloud};
pub use trips::list_trips;
pub use wipe::{commit_reclaim, plan_reclaim};
