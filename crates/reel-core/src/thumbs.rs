//! Poster-frame thumbnails, generated with ffmpeg and cached by content id, so a
//! clip is decoded once — and a clip on a card shares its poster with the same
//! clip after import, since the id is content-addressed.

use crate::config::Config;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Cache path for a clip's poster, keyed by its stable content id.
pub fn poster_path(cfg: &Config, fileid: &str) -> PathBuf {
    cfg.cache_dir.join("thumbs").join(format!("{fileid}.jpg"))
}

fn nonempty_file(p: &Path) -> bool {
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// Extract one representative frame from `clip` as a JPEG, cached at
/// `poster_path`. Returns the cached path, or `None` if ffmpeg is missing or
/// fails (the UI then shows a placeholder — never an error).
pub fn ensure_poster(cfg: &Config, clip: &Path, fileid: &str) -> Option<PathBuf> {
    let out = poster_path(cfg, fileid);
    if nonempty_file(&out) {
        return Some(out);
    }
    std::fs::create_dir_all(out.parent()?).ok()?;

    // Fast input-seek ~1s in to skip black leader frames; fall back to the very
    // start for clips shorter than that.
    for seek in ["1", "0"] {
        let ok = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-ss", seek])
            .arg("-i")
            .arg(clip)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=512:-2",
                "-pix_fmt",
                "yuvj420p",
                "-q:v",
                "3",
            ])
            .arg(&out)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok && nonempty_file(&out) {
            return Some(out);
        }
    }
    None
}

/// A poster as a `data:image/jpeg;base64,…` URI the webview can show inline.
pub fn poster_data_uri(cfg: &Config, clip: &Path, fileid: &str) -> Option<String> {
    let p = ensure_poster(cfg, clip, fileid)?;
    let bytes = std::fs::read(&p).ok()?;
    Some(format!("data:image/jpeg;base64,{}", base64(&bytes)))
}

/// Minimal standard base64 (no line breaks) — a tiny need not worth a dep.
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b1 = c.first().copied().unwrap_or(0);
        let b2 = c.get(1).copied().unwrap_or(0);
        let b3 = c.get(2).copied().unwrap_or(0);
        let n = ((b1 as u32) << 16) | ((b2 as u32) << 8) | (b3 as u32);
        s.push(T[((n >> 18) & 63) as usize] as char);
        s.push(T[((n >> 12) & 63) as usize] as char);
        s.push(if c.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
