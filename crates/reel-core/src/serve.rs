//! A tiny static file server for clips, behind the loopback HTTP server.
//!
//! Pure and dependency-free so it's unit-tested headlessly: it scope-guards every
//! request to the library, honours a single HTTP `Range` (what a `<video>`
//! element sends to seek), and caps each response to 1 MiB so memory stays
//! bounded no matter how large the master. The conventions mirror Tauri's own
//! asset protocol (206 partial content, `Content-Range`, `bytes */len` on an
//! unsatisfiable range). The Tauri layer (`clip_server`) serves these over
//! `http://127.0.0.1` because WebKitGTK can't play a custom URI scheme.

use crate::media::under;
use std::path::Path;

/// A serving decision: the Tauri layer maps these onto HTTP headers and streams
/// `send` from the file itself. Streaming (rather than buffering a body) keeps
/// memory bounded *and* lets the response carry a real `Content-Length` — a
/// `<video>` needs that to treat the clip as a seekable file instead of an
/// endless stream (without it GStreamer reports duration unknown / seekable no,
/// so playback ends the instant the transfer does).
pub struct ClipResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub accept_ranges: bool,
    /// `bytes start-end/total` for a 206, or `bytes */total` for a 416.
    pub content_range: Option<String>,
    /// Inclusive byte range to stream from the file, or `None` for an empty body
    /// (an error, a 416, or a zero-length file).
    pub send: Option<(u64, u64)>,
}

impl ClipResponse {
    fn err(status: u16) -> Self {
        ClipResponse {
            status,
            content_type: "text/plain",
            accept_ranges: false,
            content_range: None,
            send: None,
        }
    }
}

/// MIME by extension. `.lrf`/`.lrv` are H.264 in mp4-ish containers, so they're
/// forced to `video/mp4` — that's the whole trick that lets the webview play a
/// camera's native proxy despite its odd extension.
pub fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4" | "mov" | "m4v" | "lrf" | "lrv") => "video/mp4",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// Parse the first range of a `Range: bytes=…` header into inclusive `(start,
/// end)`. Supports `a-b`, `a-`, and `-suffix`; `None` on malformed input.
fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let first = spec.split(',').next()?.trim();
    let (a, b) = first.split_once('-')?;
    if a.is_empty() {
        // suffix range: the last N bytes
        let n: u64 = b.trim().parse().ok()?;
        if n == 0 {
            return None;
        }
        return Some((len.saturating_sub(n), len.saturating_sub(1)));
    }
    let start: u64 = a.trim().parse().ok()?;
    let end: u64 = if b.trim().is_empty() {
        len.saturating_sub(1)
    } else {
        b.trim().parse().ok()?
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

/// Decide how to serve `target` (confined to `lib`), honouring an optional
/// `Range`. Returns 403 outside the library, 404 if missing, 416 for an
/// unsatisfiable range, 206 for a partial range, and 200 for a whole file. No
/// bytes are read here — the caller streams `send` straight from the file.
pub fn serve_clip(lib: &Path, target: &Path, range: Option<&str>) -> ClipResponse {
    if !under(target, lib) {
        return ClipResponse::err(403);
    }
    let len = match std::fs::metadata(target) {
        Ok(m) if m.is_file() => m.len(),
        _ => return ClipResponse::err(404),
    };
    let ct = mime_for(target);

    // Empty file: nothing to range over.
    if len == 0 {
        return ClipResponse {
            status: 200,
            content_type: ct,
            accept_ranges: true,
            content_range: None,
            send: None,
        };
    }

    match range.and_then(|r| parse_range(r, len)) {
        // Unsatisfiable range (start past EOF).
        Some((start, _)) if start >= len => ClipResponse {
            status: 416,
            content_type: ct,
            accept_ranges: true,
            content_range: Some(format!("bytes */{len}")),
            send: None,
        },
        // A partial range: exactly what was asked for, clamped to EOF. No cap —
        // the caller streams it from disk, so a long range costs no memory.
        Some((start, end_req)) => {
            let end = end_req.min(len - 1);
            ClipResponse {
                status: 206,
                content_type: ct,
                accept_ranges: true,
                content_range: Some(format!("bytes {start}-{end}/{len}")),
                send: Some((start, end)),
            }
        }
        // No (or unparseable) range: the whole file, carrying a real
        // Content-Length so the player can seek.
        None => ClipResponse {
            status: 200,
            content_type: ct,
            accept_ranges: true,
            content_range: None,
            send: Some((0, len - 1)),
        },
    }
}
