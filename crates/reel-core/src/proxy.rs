//! On-demand review proxies, cached under `<trip>/.proxies/`.
//!
//! Two reasons a clip needs one: the GUI plays in a webview, and (a) native DJI
//! `.LRF` / GoPro `.LRV` proxies are mp4s carrying a *second* video stream (an
//! MJPEG thumbnail) plus telemetry data tracks that WebKitGTK's demuxer chokes
//! on; (b) a bare master is often HEVC, which the webview can't decode. So:
//!
//!   - a native proxy present → **remux** it (`-map 0:v:0 -map 0:a? -c copy`) to a
//!     clean single-stream H.264 mp4. No re-encode — ~0.1s even for a long clip.
//!   - no native proxy → **transcode** the master to a 720p H.264 mp4.
//!
//! Either way the result is one cached, webview-friendly file per master.

use crate::config::Config;
use crate::media::{is_photo, native_proxy_of, quick_fileid, rel_stem, under};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FF_BASE: &[&str] = &["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"];

/// Return a cached, webview-playable proxy for `master` in `trip`, building it if
/// absent. Prefers remuxing a native proxy (fast) over transcoding the master.
/// The master must live under the library (the path comes from the UI).
pub fn ensure_proxy(cfg: &Config, trip: &str, master: &Path) -> Result<PathBuf, String> {
    if !under(master, &cfg.lib) {
        return Err("clip is outside the library".into());
    }
    // A photo needs no proxy — it's shown directly as an image.
    if is_photo(master) {
        return Ok(master.to_path_buf());
    }
    let dir = cfg.lib.join(trip);
    let out = dir
        .join(".proxies")
        .join(format!("{}.mp4", rel_stem(master, &dir)));
    build_proxy(master, &out)
}

/// A card-preview proxy for a clip still on the card, cached by content id under
/// the cache dir (a card clip has no trip). Guarded to the same roots the clip
/// server streams from, so only a real card/library path is ever transcoded.
pub fn ensure_card_proxy(cfg: &Config, master: &Path) -> Result<PathBuf, String> {
    if !cfg.clip_roots().iter().any(|r| under(master, r)) {
        return Err("clip is outside the library or a card".into());
    }
    // A photo needs no proxy — it's shown directly as an image.
    if is_photo(master) {
        return Ok(master.to_path_buf());
    }
    let out = cfg.card_proxy_path(&quick_fileid(master));
    build_proxy(master, &out)
}

/// Build a clean, single-stream, webview-playable proxy for `master` at `out`,
/// or return it if already cached. Prefers remuxing a native `.LRF`/`.LRV` (fast,
/// no re-encode) over transcoding the master to 720p H.264.
fn build_proxy(master: &Path, out: &Path) -> Result<PathBuf, String> {
    if out.is_file() {
        return Ok(out.to_path_buf());
    }

    // Prefer a native proxy as the source: it's small, already 720p H.264, and
    // just needs its extra streams stripped. Else fall back to the master.
    let native = native_proxy_of(master);
    let source = native.as_deref().unwrap_or(master);
    if !source.is_file() {
        return Err(format!("nothing to build from: {}", source.display()));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("couldn't make proxy dir: {e}"))?;
    }

    // Temp sibling → an interrupted build never leaves a half proxy that looks done.
    let tmp = out.with_extension("partial.mp4");
    let _ = std::fs::remove_file(&tmp);

    let mut cmd = Command::new("ffmpeg");
    cmd.args(FF_BASE).arg(source);
    // Map only the primary video + optional audio, dropping the MJPEG thumbnail,
    // telemetry data tracks, and timecode track that break the webview.
    cmd.args(["-map", "0:v:0", "-map", "0:a?", "-write_tmcd", "0"]);
    if native.is_some() {
        // Remux the native proxy as-is (already 720p H.264) — no re-encode.
        cmd.args(["-c", "copy", "-movflags", "+faststart"]);
    } else {
        // Transcode the master to a 720-box H.264 mp4. force_divisible_by=2 keeps
        // both dimensions even (x264 rejects odd sizes); yuv420p + AAC stay broadly
        // decodable; faststart puts the moov up front for range streaming.
        cmd.args([
            "-vf",
            "scale=w=720:h=720:force_original_aspect_ratio=decrease:force_divisible_by=2",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "28",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-movflags",
            "+faststart",
        ]);
    }
    let output = cmd
        .arg(&tmp)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg won't start: {e} (is it installed?)"))?;

    if output.status.success() && tmp.is_file() {
        std::fs::rename(&tmp, out).map_err(|e| format!("couldn't save proxy: {e}"))?;
        Ok(out.to_path_buf())
    } else {
        let _ = std::fs::remove_file(&tmp);
        // Surface ffmpeg's own reason (last stderr line) so a failure is diagnosable.
        let stderr = String::from_utf8_lossy(&output.stderr);
        match stderr.trim().lines().last().map(str::trim) {
            Some(line) if !line.is_empty() => Err(format!("ffmpeg: {line}")),
            _ => Err("ffmpeg couldn't build a proxy for this clip".into()),
        }
    }
}
