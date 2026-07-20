//! Loopback HTTP server that streams library clips to the `<video>` element.
//!
//! WebKitGTK's GStreamer media backend cannot load media from a custom URI
//! scheme registered by the app — WebKit bug 146351, still open, so a `<video>`
//! pointed at `reelclip://…` never even fetches (no request reaches us, no
//! codec runs). It *does* handle plain `http://127.0.0.1` via `souphttpsrc`,
//! range requests and all. So we serve clips over a tiny loopback listener
//! instead of a custom scheme.
//!
//! The range/scope/MIME logic lives in reel-core (`serve::serve_clip`, unit
//! tested); this module only binds an ephemeral loopback port and adapts that
//! response onto `tiny_http`. It listens on 127.0.0.1 only and refuses anything
//! outside the library, so it exposes nothing a same-user process couldn't
//! already read directly.

use reel_core::serve::serve_clip;
use reel_core::Config;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_http::{Header, Response, Server, StatusCode};

/// Start the clip server on an OS-assigned loopback port and return its base
/// URL (`http://127.0.0.1:<port>`). Spawns detached worker threads that live for
/// the process; the UI appends `/<percent-encoded-abs-path>` to reach a clip.
pub fn start() -> std::io::Result<String> {
    let server = Server::http("127.0.0.1:0").map_err(|e| std::io::Error::other(e.to_string()))?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .ok_or_else(|| std::io::Error::other("no loopback port"))?;
    // The allowed roots (library + card mount) are fixed for the process; the
    // card mount parent is stable across insert/remove, so resolve them once.
    let roots = Arc::new(Config::from_env().clip_roots());
    let server = Arc::new(server);
    // A few workers so overlapping range requests (a seek fires several) don't
    // serialize behind one another.
    for _ in 0..4 {
        let server = Arc::clone(&server);
        let roots = Arc::clone(&roots);
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                handle(&roots, req);
            }
        });
    }
    Ok(format!("http://127.0.0.1:{port}"))
}

fn handle(roots: &[PathBuf], req: tiny_http::Request) {
    let target = decode_path(req.url());
    let range = req
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("range"))
        .map(|h| h.value.as_str().to_string());

    let r = serve_clip(roots, &target, range.as_deref());

    let mut headers = vec![header("Content-Type", r.content_type)];
    if r.accept_ranges {
        headers.push(header("Accept-Ranges", "bytes"));
    }
    if let Some(cr) = &r.content_range {
        headers.push(header("Content-Range", cr));
    }
    let status = StatusCode(r.status);

    // Stream the chosen byte range straight from the file with an explicit
    // length, so tiny_http sends Content-Length instead of a chunked body — a
    // <video> needs that to seek (a chunked clip reads as an endless stream and
    // ends the moment the transfer does). tiny_http chunks any body over 32 KiB
    // by default, so raise the threshold past any file. `send` is None for
    // errors / 416 / empty.
    let (reader, n) = r
        .send
        .and_then(|(start, end)| open_range(&target, start, end))
        .unwrap_or_else(|| (empty_body(), 0));
    let resp = Response::new(status, headers, reader, Some(n as usize), None)
        .with_chunked_threshold(usize::MAX);
    let _ = req.respond(resp);
}

/// Open `target` and position a reader over the inclusive `[start, end]` range,
/// with its byte count. `None` if the file can't be opened/seeked (it vanished
/// between the stat and here) — the caller then sends an empty body.
fn open_range(target: &Path, start: u64, end: u64) -> Option<(Box<dyn Read + Send>, u64)> {
    let mut f = File::open(target).ok()?;
    f.seek(SeekFrom::Start(start)).ok()?;
    let n = end + 1 - start;
    Some((Box::new(f.take(n)), n))
}

fn empty_body() -> Box<dyn Read + Send> {
    Box::new(std::io::empty())
}

/// Turn a request URL (`/<percent-encoded-abs-path>`) back into the absolute
/// filesystem path. The UI `encodeURIComponent`s the whole path, but soup may
/// pre-decode `%2F`; strip only the one authority slash and re-assert a leading
/// slash so we always land on an absolute path (`serve_clip` then scope-guards).
fn decode_path(url: &str) -> PathBuf {
    let raw = url.split('?').next().unwrap_or(url);
    let mut path = percent_decode(raw.strip_prefix('/').unwrap_or(raw));
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    PathBuf::from(path)
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

/// Minimal percent-decode (`%XX` → byte); invalid escapes pass through verbatim.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
