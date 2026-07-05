//! Files on disk: camera classification, capture time, and a stable content id.

use crate::model::ClipHealth;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

/// Camera bucket. Masters, proxies and thumbs of one camera share a bucket so a
/// proxy stays beside its master.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Dji,
    Gopro,
    Iphone,
    Misc,
}

impl Kind {
    /// Folder name under `<trip>/<person>/`.
    pub fn dir(self) -> &'static str {
        match self {
            Kind::Dji => "dji",
            Kind::Gopro => "gopro",
            Kind::Iphone => "iphone",
            Kind::Misc => "misc",
        }
    }
}

/// Classify by filename: `DJI_*`, `G[XHL]*` (GoPro masters/proxies/thumbs),
/// `IMG_*` (iPhone), else misc.
pub fn kind_of(path: &Path) -> Kind {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let b = name.as_bytes();
    if name.starts_with("DJI_") {
        Kind::Dji
    } else if b.first() == Some(&b'G') && matches!(b.get(1), Some(b'X' | b'H' | b'L')) {
        Kind::Gopro
    } else if name.starts_with("IMG_") {
        Kind::Iphone
    } else {
        Kind::Misc
    }
}

pub const MASTER_EXT: &[&str] = &["mp4", "mov", "mkv"];
pub const MEDIA_EXT: &[&str] = &["mp4", "mov", "mkv", "lrf", "lrv", "thm"];

pub fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Capture time as epoch seconds. mtime is the capture time on the card and is
/// preserved on copy, so it's stable across the pipeline (action-cam ffprobe
/// creation_time is often zeroed). Unknown → 0.
pub fn captured_at(p: &Path) -> i64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

const CHUNK: u64 = 4 * 1024 * 1024;

/// Stable content id that survives renames without reading whole files: the byte
/// size plus a sha1 of (size + first 4 MiB + last 4 MiB). Constant ~8 MiB read
/// per file regardless of length. Byte-compatible with the original script's id.
pub fn fileid_of(p: &Path) -> io::Result<String> {
    let size = fs::metadata(p)?.len();
    let n = CHUNK.min(size);
    let mut f = File::open(p)?;

    let mut head = vec![0u8; n as usize];
    f.read_exact(&mut head)?;
    let tail = if size > CHUNK {
        f.seek(SeekFrom::Start(size - n))?;
        let mut t = vec![0u8; n as usize];
        f.read_exact(&mut t)?;
        t
    } else {
        head.clone()
    };

    let mut h = Sha1::new();
    h.update(size.to_string().as_bytes());
    h.update([0u8]);
    h.update(&head);
    h.update(&tail);
    Ok(format!("{}-{}", size, hex::encode(h.finalize())))
}

/// A cheap thumbnail-cache id from a single stat — no content read. Unlike
/// `fileid_of` it isn't content-addressed (won't survive a rename or match a
/// card copy's poster), but computing it is a `stat` rather than an ~8 MiB read.
/// Review uses it for masters not on the ledger, so opening a big, un-ledgered
/// trip stays instant instead of hashing hundreds of clips up front.
pub fn quick_fileid(p: &Path) -> String {
    let (size, mtime) = fs::metadata(p)
        .map(|m| {
            let mt = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (m.len(), mt)
        })
        .unwrap_or((0, 0));
    let mut h = Sha1::new();
    h.update(p.to_string_lossy().as_bytes());
    h.update([0u8]);
    h.update(size.to_le_bytes());
    h.update(mtime.to_le_bytes());
    format!("q{}", hex::encode(h.finalize()))
}

/// Clips shorter than this are accidental taps — too brief to review, and short
/// enough that the webview's media pipeline can stall trying to open them.
const MIN_PLAYABLE_SECS: f64 = 0.6;

/// A quick ffprobe verdict on a clip (reads only headers, tens of ms): does it
/// have a video stream, and is it long enough to bother loading? Lets the player
/// flag empty / blip / corrupt clips immediately instead of waiting out a doomed
/// load. Errs toward `ok` when ffprobe is missing so a probe can't block playback.
pub fn clip_health(path: &Path) -> ClipHealth {
    let ok = |duration| ClipHealth {
        ok: true,
        tag: String::new(),
        reason: String::new(),
        duration,
    };
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("this clip");
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1",
        ])
        .arg(path)
        .output();
    let text = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        Ok(_) => {
            return ClipHealth {
                ok: false,
                tag: "unreadable".into(),
                reason: format!("“{name}” looks corrupt — nothing to play."),
                duration: 0.0,
            }
        }
        Err(_) => return ok(0.0), // no ffprobe → don't block on a probe we can't run
    };
    let has_video = text.lines().any(|l| l.trim() == "codec_type=video");
    let duration = text
        .lines()
        .find_map(|l| l.strip_prefix("duration="))
        .and_then(|d| d.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    if !has_video {
        return ClipHealth {
            ok: false,
            tag: "empty".into(),
            reason: format!("“{name}” has no video to play."),
            duration,
        };
    }
    if duration > 0.0 && duration < MIN_PLAYABLE_SECS {
        return ClipHealth {
            ok: false,
            tag: "brief".into(),
            reason: format!("“{name}” is too brief to play ({duration:.1}s)."),
            duration,
        };
    }
    ok(duration)
}

/// `(epoch, path)` for every master under `roots`, capture-ordered.
pub fn masters_under(roots: &[PathBuf]) -> Vec<(i64, PathBuf)> {
    let mut v = Vec::new();
    for r in roots {
        for e in walkdir::WalkDir::new(r).into_iter().filter_map(|e| e.ok()) {
            if e.file_type().is_file() && has_ext(e.path(), MASTER_EXT) {
                v.push((captured_at(e.path()), e.path().to_path_buf()));
            }
        }
    }
    v.sort();
    v
}

fn in_excluded_dir(p: &Path, base: &Path) -> bool {
    p.strip_prefix(base)
        .map(|rel| {
            rel.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some("clips") | Some("_sheets") | Some(".proxies")
                )
            })
        })
        .unwrap_or(false)
}

/// Masters inside a trip (excluding derived dirs), capture-ordered.
pub fn masters_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<(i64, PathBuf)> = Vec::new();
    for e in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = e.path();
        if e.file_type().is_file() && has_ext(p, MASTER_EXT) && !in_excluded_dir(p, dir) {
            v.push((captured_at(p), p.to_path_buf()));
        }
    }
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}

/// Is `target` inside `root`? Both are canonicalized so symlinks and `..` can't
/// dodge the check — used to keep the clip server and proxy builder confined to
/// the library. A lexical fallback covers the case where `target` doesn't exist
/// yet (canonicalize fails) but its prefix clearly sits under `root`.
pub fn under(target: &Path, root: &Path) -> bool {
    match (target.canonicalize(), root.canonicalize()) {
        (Ok(t), Ok(r)) => t.starts_with(r),
        _ => root
            .canonicalize()
            .ok()
            .map(|r| target.starts_with(&r) || target.starts_with(root))
            .unwrap_or_else(|| target.starts_with(root)),
    }
}

fn first_existing(dir: &Path, names: &[String]) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// A camera's native low-res proxy beside a master, if one was recorded: DJI's
/// `.LRF` or GoPro's `GL######.LRV`. These ride the master's exact timeline, so
/// they're what `review` scrubs for speed while marks stay in master seconds.
/// (Cards write them uppercase; a lowercase fallback covers migrated footage.)
pub fn native_proxy_of(master: &Path) -> Option<PathBuf> {
    let name = master.file_name()?.to_str()?;
    let dir = master.parent()?;
    let stem = master.file_stem()?.to_str()?;
    if !has_ext(master, &["mp4"]) {
        return None;
    }
    // DJI_0001.MP4 -> DJI_0001.LRF
    if name.starts_with("DJI_") {
        return first_existing(dir, &[format!("{stem}.LRF"), format!("{stem}.lrf")]);
    }
    // GX010123.MP4 / GH010123.MP4 -> GL010123.LRV
    let b = name.as_bytes();
    if b.first() == Some(&b'G')
        && matches!(b.get(1), Some(b'X' | b'H'))
        && stem.len() == 8
        && stem.as_bytes()[2..8].iter().all(u8::is_ascii_digit)
    {
        let digits = &stem[2..8];
        return first_existing(dir, &[format!("GL{digits}.LRV"), format!("GL{digits}.lrv")]);
    }
    None
}

/// Collision-safe stem for derived files: the master's path relative to the
/// trip, extension dropped, `/` → `__`. Matches the script's `rel_stem` so the
/// GUI and CLI name generated proxies/sheets/clips identically.
pub fn rel_stem(master: &Path, trip: &Path) -> String {
    let rel = master.strip_prefix(trip).unwrap_or(master);
    rel.with_extension("").to_string_lossy().replace('/', "__")
}
