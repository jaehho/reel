//! reel-core — the engine behind the reel GUI.
//!
//! Pure Rust, no GUI dependency, so it's unit-tested headlessly. Ports the
//! footage logic from the original `reel` script: card/session survey, the
//! content-id dedup ledger, trip discovery and state, and poster thumbnails.
//! Mutating phases (import, cut, pool sync, wipe) land here too as they're built.
#![allow(dead_code)]

pub mod archive;
pub mod cards;
pub mod config;
pub mod cut;
pub mod import;
pub mod ledger;
pub mod media;
pub mod model;
pub mod proxy;
pub mod push;
pub mod rclone;
pub mod review;
pub mod serve;
pub mod sessions;
pub mod thumbs;
pub mod trips;
pub mod wipe;

pub use archive::{commit_archive, plan_archive};
pub use cards::scan_card;
pub use config::Config;
pub use cut::cut_trip;
pub use import::import_window;
pub use model::{
    ArchivePlan, ArchiveProgress, ArchiveResult, CardInfo, ClipHealth, ClipRef, CutProgress,
    CutResult, ImportProgress, ImportResult, Mark, Playlist, PushPhase, PushProgress, PushResult,
    ReclaimPlan, ReclaimResult, ReviewClip, Session, Share, Trip, TripState, WipePhase,
    WipeProgress,
};
pub use proxy::ensure_proxy;
pub use push::push_trip;
pub use review::{review_playlist, save_marks};
pub use trips::list_trips;
pub use wipe::{commit_reclaim, plan_reclaim};
