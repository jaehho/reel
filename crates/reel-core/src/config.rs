//! Runtime configuration, resolved from the same env knobs as the original
//! `reel` script so the GUI and any leftover CLI agree on paths.

use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    /// Local workspace root (`REEL_LIB`, default `~/Videos`).
    pub lib: PathBuf,
    /// rclone destination for the shared cloud (`REEL_REMOTE`).
    pub remote: String,
    /// Your folder name inside a trip / the cloud (`REEL_USER`).
    pub user: String,
    /// State dir holding the import ledger and logs.
    pub state_dir: PathBuf,
    /// Cache dir for generated thumbnails (`XDG_CACHE_HOME`/reel, else ~/.cache/reel).
    pub cache_dir: PathBuf,
    /// Seconds between captures that starts a new session (`REEL_SESSION_GAP`).
    pub session_gap: i64,
    /// Explicit card roots, overriding auto-detection.
    pub dji_sd: Option<PathBuf>,
    pub gopro_sd: Option<PathBuf>,
    /// User whose `/run/media/<user>/*/DCIM` mounts we scan for cards.
    pub media_user: String,
}

fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"))
}

impl Config {
    pub fn from_env() -> Self {
        let user = env::var("REEL_USER")
            .or_else(|_| env::var("USER"))
            .unwrap_or_else(|_| "user".into());
        let lib = env::var_os("REEL_LIB")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("Videos"));
        let state_dir = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/state"))
            .join("reel");
        let cache_dir = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".cache"))
            .join("reel");
        Config {
            lib,
            remote: env::var("REEL_REMOTE").unwrap_or_else(|_| "nextcloud:Reels".into()),
            media_user: env::var("USER").unwrap_or_else(|_| user.clone()),
            user,
            state_dir,
            cache_dir,
            session_gap: env::var("REEL_SESSION_GAP")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(21600),
            dji_sd: env::var_os("DJI_SD").map(PathBuf::from),
            gopro_sd: env::var_os("GOPRO_SD").map(PathBuf::from),
        }
    }

    pub fn ledger_path(&self) -> PathBuf {
        self.state_dir.join("imported.tsv")
    }

    /// Filesystem roots the loopback clip server is allowed to stream from: the
    /// library, the card-preview proxy cache, and wherever a card mounts (the
    /// `/run/media/<user>` parent, or the `DJI_SD`/`GOPRO_SD` overrides). Card
    /// preview plays clips straight off the card and its remuxed proxies out of the
    /// cache, so the scope guard has to admit those paths — still same-user-readable
    /// locations, never an arbitrary path. (Trip proxies live under the library.)
    pub fn clip_roots(&self) -> Vec<PathBuf> {
        let mut r = vec![self.lib.clone(), self.cache_dir.join("proxies")];
        if let Some(p) = &self.dji_sd {
            r.push(p.clone());
        }
        if let Some(p) = &self.gopro_sd {
            r.push(p.clone());
        }
        r.push(PathBuf::from("/run/media").join(&self.media_user));
        r
    }

    /// Cache for on-demand *card* preview proxies, keyed by content id so it
    /// survives a card reinsert and is shared across sessions. (Trip proxies live
    /// beside their master under `<trip>/.proxies`; a card clip has no trip.)
    pub fn card_proxy_path(&self, fileid: &str) -> PathBuf {
        self.cache_dir.join("proxies").join(format!("{fileid}.mp4"))
    }

    /// Content ids of footage the user permanently deleted. Kept beside the
    /// ledger so a killed clip still on a card reads as "discarded" and is never
    /// re-offered for import.
    pub fn tombstones_path(&self) -> PathBuf {
        self.state_dir.join("deleted.tsv")
    }

    /// Per-trip sync baseline: which clips *should* be in the shared cloud
    /// (`person/camera/base` + size), one file per trip so a rename is a file
    /// rename with no row rewrites. Diffed against the live cloud listing to work
    /// out a trip's sync status; maintained by every cloud op.
    pub fn synced_dir(&self) -> PathBuf {
        self.state_dir.join("synced")
    }
    pub fn base_path(&self, trip: &str) -> PathBuf {
        self.synced_dir().join(format!("{trip}.tsv"))
    }

    /// Per-trip cache of the last cloud listing we fetched, so the dashboard can
    /// show cloud-side drift (and a "checked N ago") without a network hit.
    pub fn cloud_cache_path(&self, trip: &str) -> PathBuf {
        self.state_dir.join("cloud").join(format!("{trip}.tsv"))
    }

    /// Directory of per-trip share caches (one `<trip>.tsv` each). Its union is the
    /// network-free "friends you share with" list.
    pub fn share_cache_dir(&self) -> PathBuf {
        self.state_dir.join("shares")
    }

    /// Per-trip cache of who the trip's cloud folder is shared with, from the last
    /// Sharing-panel (or background) fetch — lets the dashboard show a "shared
    /// with N" chip without a network hit (mirrors `cloud_cache_path`).
    pub fn share_cache_path(&self, trip: &str) -> PathBuf {
        self.share_cache_dir().join(format!("{trip}.tsv"))
    }

    /// Cloud ops owed because the remote was unreachable when they ran (a move, a
    /// rename, or a whole-trip purge — the ones the local/base/cloud compare can't
    /// re-derive on its own). Replayed on the next sync.
    pub fn pending_path(&self) -> PathBuf {
        self.state_dir.join("pending.tsv")
    }
}
