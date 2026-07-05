//! Runtime configuration, resolved from the same env knobs as the original
//! `reel` script so the GUI and any leftover CLI agree on paths.

use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    /// Local workspace root (`REEL_LIB`, default `~/Videos`).
    pub lib: PathBuf,
    /// rclone destination for the shared pool (`REEL_REMOTE`).
    pub remote: String,
    /// Your folder name inside a trip / the pool (`REEL_USER`).
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
}
