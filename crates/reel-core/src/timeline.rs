//! Hand a trip's marks to Kdenlive as one editable timeline (`.kdenlive`).
//!
//! The counterpart to `cut`, not a replacement for it. `cut` writes a standalone
//! file per mark — the thing you hand someone. This writes a *project*: every mark
//! becomes a clip on one timeline, in capture order, referencing the **master**
//! with in/out points rather than the cut file. That's the difference that earns
//! it: an edge you set in reel stays draggable in Kdenlive, because the whole
//! master is still sitting behind the cut. A cut file has no handles — trimmed is
//! trimmed. So `cut` is for sharing and this is for editing, and a trip normally
//! wants both.
//!
//! The format is MLT XML, which is what a `.kdenlive` file is. Facts below are
//! load-bearing and were *measured* against MLT 7.40 and Kdenlive 26.04, not
//! assumed (see `tests/engine.rs` and the notes in TODO.md):
//!   - `<entry in= out=>` are **frame numbers into the source**, not timeline
//!     positions and not seconds.
//!   - `out` is **inclusive**: a segment spans `out - in + 1` frames.
//!   - A Kdenlive track is a `<tractor>` over *two* `<playlist>`s, and the split
//!     between picture and sound is the `hide=` attribute on its inner tracks.
//!     Get those backwards and the render is silent, which reading the XML won't
//!     tell you.
//!   - **Kdenlive 23.08+ stores the timeline as a *sequence*** — a tractor whose
//!     `id` is a UUID, carrying `kdenlive:producer_type=17`, listed in the bin,
//!     and pointed at by `docproperties.activetimeline`. The older flat layout is
//!     valid MLT and `melt` renders it perfectly, but Kdenlive **will not open
//!     it**: it hangs on load. That gap is the whole reason this module is shaped
//!     the way it is, and it is why `melt` alone is not an acceptance test here.
//!   - Guides live in `kdenlive:sequenceproperties.guides` **on the sequence
//!     tractor**. `docproperties.guides` is a legacy upgrade target only; writing
//!     there silently produces a timeline with no guides on the ruler.
//!   - Kdenlive refuses a **fractional frame rate on non-standard geometry** with a
//!     modal on every open, and no custom profile avoids it (see `conform`). That
//!     modal is invisible to `kdenlive --render`, which takes a different path — so
//!     even the acceptance test here can't see it, and this one is on trust.
//!
//! Timing comes from the project's frame rate, which is normally the footage's own,
//! so a mark at 70.920 s lands on the frame the player was showing. `conform` is the
//! exception, and it moves every frame number together.

use crate::config::Config;
use crate::media::captured_at;
use crate::model::{Mark, TimelineResult};
use crate::review::read_marks;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Kdenlive keys clips and sequences by UUID. These are derived from a stable seed
/// rather than randomised, so rebuilding a trip's timeline produces a
/// byte-identical file — re-running `edit` doesn't churn the project, and a diff
/// between two builds shows only what actually changed.
fn uuid_from(seed: &str) -> String {
    let hex = hex::encode(Sha1::digest(seed.as_bytes()));
    // Shape as a v4 UUID: version nibble `4`, variant nibble in 8..=b. Kdenlive
    // parses these with QUuid, which rejects anything malformed and then silently
    // drops the clip.
    let variant = match &hex[16..17] {
        "0" | "1" | "2" | "3" | "8" => "8",
        "4" | "5" | "9" => "9",
        "6" | "7" | "a" => "a",
        _ => "b",
    };
    format!(
        "{{{}-{}-4{}-{}{}-{}}}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        variant,
        &hex[17..20],
        &hex[20..32]
    )
}

fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// The app's own sentinel for an unnamed highlight — `h` writes this label, and the
/// UI already treats it as "no name" everywhere it renders a mark. The timeline
/// follows the same rule so a ruler full of guides reading "hl" never happens.
fn named(label: &str) -> Option<&str> {
    let l = label.trim();
    (!l.is_empty() && l != "hl").then_some(l)
}

/// XML attribute/text escaping. Labels are user text and masters really do have
/// spaces and ampersands in them (`26-06-20 14-36-45 4297.mov`), so this is not
/// theoretical.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 forbids these outright; a stray control byte in a label would
            // otherwise produce a file no parser will read.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Escaping for a JSON string that is *itself* inside an XML attribute-free text
/// node (the guides property). Both layers apply: JSON first, then the caller's
/// `xml_escape`.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The video format a timeline is built in. One project has one profile, so a trip
/// mixing formats (real ones do — 4K DJI at 23.976 alongside a 30 fps phone clip)
/// conforms everything to the majority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub sar_num: u32,
    pub sar_den: u32,
}

impl Default for Profile {
    /// 1080p30 — only reached when nothing could be probed, in which case the
    /// timeline is empty anyway and the profile is cosmetic.
    fn default() -> Self {
        Profile {
            width: 1920,
            height: 1080,
            fps_num: 30,
            fps_den: 1,
            sar_num: 1,
            sar_den: 1,
        }
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a.max(1)
    } else {
        gcd(b, a % b)
    }
}

impl Profile {
    pub fn fps(&self) -> f64 {
        if self.fps_den == 0 {
            30.0
        } else {
            self.fps_num as f64 / self.fps_den as f64
        }
    }

    /// Display aspect, reduced. Computed from the real pixel dimensions rather than
    /// assumed 16:9 — the library has 1080x1920 portrait drone footage in it, which
    /// a hardcoded 16:9 would letterbox into a sliver.
    pub fn dar(&self) -> (u32, u32) {
        let w = self.width * self.sar_num.max(1);
        let h = self.height * self.sar_den.max(1);
        let g = gcd(w, h);
        (w / g.max(1), h / g.max(1))
    }

    /// Short human description for the toast and the `<profile description=…>`.
    pub fn describe(&self) -> String {
        let fps = self.fps();
        // 23.976 and 29.97 want the decimals; 30 and 24 don't.
        let rate = if (fps - fps.round()).abs() < 0.001 {
            format!("{}", fps.round() as i64)
        } else {
            format!("{fps:.2}")
        };
        format!("{}×{} · {} fps", self.width, self.height, rate)
    }
}

/// One mark resolved onto the timeline: which master, which frames of it, and what
/// to call it. `out_f` is inclusive, matching MLT.
#[derive(Clone, Debug)]
pub struct Segment {
    pub master: PathBuf,
    /// Master basename — the bin clip's name.
    pub name: String,
    /// The mark's label, or empty when unnamed (`hl`).
    pub label: String,
    pub in_f: i64,
    pub out_f: i64,
}

impl Segment {
    pub fn frames(&self) -> i64 {
        self.out_f - self.in_f + 1
    }
    /// What the ruler guide says: the name you gave the mark, else the clip it came
    /// from. A guide per segment is the "marks copied into the GUI" part.
    pub fn guide(&self) -> String {
        named(&self.label)
            .map(str::to_string)
            .unwrap_or_else(|| self.name.clone())
    }
}

/// A master as the bin sees it: the whole file, however long it is.
#[derive(Clone, Debug)]
pub struct Source {
    pub master: PathBuf,
    pub name: String,
    pub length_f: i64,
    /// Does it carry audio? A still-image capture or a silent clip gets no audio
    /// entry, rather than an audio clip that renders as silence.
    pub has_audio: bool,
    /// The proxy reel already built for this master under `<trip>/.proxies/`, when
    /// there is one. These are h264 720p remuxes of the camera's own low-res track
    /// and — critically — they carry the **same frame count and rate** as the
    /// master, so they can stand in frame-for-frame. Handing them to Kdenlive as
    /// its proxy is the difference between scrubbing a 12 MB h264 file and
    /// decoding 113 MB of 4K HEVC per clip.
    pub proxy: Option<PathBuf>,
}

impl Source {
    /// What a producer's `resource` should point at: the proxy when we have one.
    /// Kdenlive keeps the master in `kdenlive:originalurl` and swaps back to it for
    /// rendering, so this costs nothing in output quality.
    fn resource(&self) -> &Path {
        self.proxy.as_deref().unwrap_or(&self.master)
    }
}

/// Round a timestamp in seconds to a frame index at `fps`.
fn frame_at(sec: f64, fps: f64) -> i64 {
    if !sec.is_finite() || sec <= 0.0 {
        return 0;
    }
    (sec * fps).round() as i64
}

// ---- probing ----

/// What ffprobe tells us about a master. `None` when it can't be read at all — the
/// library has a real master with no moov atom, and a timeline that references it
/// would just fail to open later, so it's dropped here with a count instead.
#[derive(Clone, Debug)]
pub struct ProbeInfo {
    pub profile: Profile,
    /// Length in seconds. Frames are derived later, against the *project* rate,
    /// which may not be this clip's own.
    pub duration: f64,
    pub has_audio: bool,
}

fn parse_ratio(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once('/').or_else(|| s.split_once(':'))?;
    let n: u32 = a.trim().parse().ok()?;
    let d: u32 = b.trim().parse().ok()?;
    (n > 0 && d > 0).then_some((n, d))
}

/// One ffprobe per master (headers only). Deliberately reads `r_frame_rate`, not
/// `avg_frame_rate`: real footage is variable-rate (a phone clip here reports
/// `30/1` base against a `22525/751` average), and the base rate is the timebase
/// the container's frame numbers actually mean.
pub fn probe(master: &Path) -> Option<ProbeInfo> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,sample_aspect_ratio",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1",
        ])
        .arg(master)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(str::trim)
    };
    let width: u32 = field("width")?.parse().ok()?;
    let height: u32 = field("height")?.parse().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let (fps_num, fps_den) = field("r_frame_rate").and_then(parse_ratio)?;
    // Real cameras report `N/A` here far more often than `1:1`; square pixels is the
    // right reading of "unspecified", not a reason to give up on the clip.
    let (sar_num, sar_den) = field("sample_aspect_ratio")
        .and_then(parse_ratio)
        .unwrap_or((1, 1));
    let profile = Profile {
        width,
        height,
        fps_num,
        fps_den,
        sar_num,
        sar_den,
    };
    // Duration in *seconds*, not frames. Frames only mean anything against a chosen
    // project rate, and that rate isn't known until every master has been probed —
    // it may even be conformed away from this clip's own rate (see `conform`).
    // `nb_frames` is skipped for the same reason it always is here: absent or wrong
    // on the variable-rate files this library is full of.
    let duration: f64 = field("duration")
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);

    // A second, cheap probe for an audio stream: whether to lay an audio clip at all.
    let has_audio = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(master)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("audio"))
        .unwrap_or(false);

    Some(ProbeInfo {
        profile,
        duration,
        has_audio,
    })
}

/// Bring a project profile to something Kdenlive will open without arguing.
///
/// Kdenlive refuses **fractional frame rates on non-standard geometry**: it will
/// happily invent a custom profile for an odd size at an integer rate, but the same
/// size at 24000/1001 raises *"The project uses a non standard framerate (23.98),
/// this will result in misplaced clips and frame offset"* — a modal, on every open.
/// Portrait drone footage (1080x1920 @ 23.98) is exactly that case, and no custom
/// profile avoids it: tested in Kdenlive's own file format, in its own profile
/// directory, under several names.
///
/// So when nothing standard matches, the rate is rounded to the nearest integer —
/// 23.976 becomes 24, a 0.1% difference, under one frame across a 30 s timeline.
/// Every frame position is then computed at *this* rate, so clips still land where
/// the marks were in time. Footage that does match a stock profile (4K landscape at
/// 23.98 is `uhd_2160p_2398`) is left exactly alone.
pub fn conform(profile: &Profile) -> (Profile, Option<String>, bool) {
    if let Some(id) = stock_profile_name(profile) {
        return (profile.clone(), Some(id), false);
    }
    if profile.fps_den <= 1 {
        // Already an integer rate — Kdenlive builds its own profile silently.
        return (profile.clone(), None, false);
    }
    let rounded = Profile {
        fps_num: profile.fps().round().max(1.0) as u32,
        fps_den: 1,
        ..profile.clone()
    };
    // The rounded rate may itself land on a stock profile; take it if so.
    let id = stock_profile_name(&rounded);
    (rounded, id, true)
}

/// Pick the project profile: the format most of the marked footage is already in.
/// Ties break toward the first master in timeline order, so a 50/50 trip is at
/// least predictable.
pub fn choose_profile(profiles: &[Profile]) -> Profile {
    let mut counts: Vec<(Profile, usize)> = Vec::new();
    for p in profiles {
        match counts.iter_mut().find(|(q, _)| q == p) {
            Some((_, n)) => *n += 1,
            None => counts.push((p.clone(), 1)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(p, _)| p)
        .unwrap_or_default()
}

/// Match a profile against a set of `(name, profile)` candidates, exactly. Pure, so
/// the matching rule is testable without depending on which MLT build is installed.
/// Ties break on name so the choice is deterministic.
pub fn match_profile(candidates: &[(String, Profile)], want: &Profile) -> Option<String> {
    let mut hits: Vec<&String> = candidates
        .iter()
        .filter(|(_, p)| p == want)
        .map(|(n, _)| n)
        .collect();
    hits.sort();
    hits.first().map(|s| s.to_string())
}

/// Parse an MLT profile file (`key=value` lines, as in `/usr/share/mlt-7/profiles`).
fn parse_profile_file(text: &str) -> Option<Profile> {
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .and_then(|v| v.trim().parse::<u32>().ok())
    };
    Some(Profile {
        width: get("width")?,
        height: get("height")?,
        fps_num: get("frame_rate_num")?,
        fps_den: get("frame_rate_den")?,
        sar_num: get("sample_aspect_num").unwrap_or(1),
        sar_den: get("sample_aspect_den").unwrap_or(1),
    })
}

/// The name of the stock MLT profile matching `want`, if there is one.
///
/// This is not cosmetic. `kdenlive:docproperties.profile` *is* the project profile:
/// point it at a name Kdenlive can't resolve and it blocks on a modal at load;
/// leave it out and Kdenlive silently falls back to its own default (PAL 720x576),
/// quietly conforming 4K footage to standard definition. Only a real name does the
/// right thing, so it's looked up rather than invented.
///
/// Coverage is better than it sounds: `uhd_2160p_2398` is exactly what the DJI
/// masters here are, and stock profiles include portrait (`vertical_hd_30`).
pub fn stock_profile_name(want: &Profile) -> Option<String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("MLT_DATA") {
        dirs.push(PathBuf::from(d).join("profiles"));
    }
    dirs.extend(
        [
            "/usr/share/mlt-7/profiles",
            "/usr/share/mlt/profiles",
            "/usr/local/share/mlt-7/profiles",
            "/usr/local/share/mlt/profiles",
        ]
        .iter()
        .map(PathBuf::from),
    );
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let candidates: Vec<(String, Profile)> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                let p = parse_profile_file(&std::fs::read_to_string(e.path()).ok()?)?;
                Some((name, p))
            })
            .collect();
        if let Some(hit) = match_profile(&candidates, want) {
            return Some(hit);
        }
    }
    None
}

// ---- XML ----

fn prop(out: &mut String, indent: &str, name: &str, value: &str) {
    out.push_str(&format!(
        "{indent}<property name=\"{}\">{}</property>\n",
        xml_escape(name),
        xml_escape(value)
    ));
}

/// One `{comment, pos, type, duration}` record. Kdenlive uses the same serializer
/// for timeline guides and clip markers, so both are built here. `pos` is in
/// **frames** (`markerlistmodel.cpp`), which is the detail worth getting wrong once.
fn marker_json(entries: &[(i64, String)]) -> String {
    let mut s = String::from("[");
    for (i, (pos, comment)) in entries.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"comment\":\"{}\",\"pos\":{},\"type\":0,\"duration\":0}}",
            json_escape(comment),
            pos
        ));
    }
    s.push(']');
    s
}

/// Serialize a whole project. Pure — no filesystem, no ffmpeg — so the structure is
/// testable headlessly, which is the only way the frame arithmetic ever gets
/// checked without rendering a video.
///
/// `sources` carries each master's **full** length, which is not derivable from the
/// segments and is the point of the whole exercise: a bin clip that stops at the
/// last mark is a clip whose edges can't be dragged outward in Kdenlive, which is
/// exactly what referencing masters instead of cut files was meant to buy.
pub fn timeline_xml(
    title: &str,
    root: &Path,
    profile: &Profile,
    profile_id: Option<&str>,
    sources: &[Source],
    segs: &[Segment],
) -> String {
    // Bin ids start at 2 — Kdenlive reserves 1 for the sequence itself.
    let bin_id =
        |m: &Path| -> usize { sources.iter().position(|s| s.master == m).unwrap_or(0) + 2 };
    let source_of = |m: &Path| sources.iter().find(|s| s.master == m);

    let total: i64 = segs.iter().map(|s| s.frames()).sum::<i64>().max(1);
    let last = total - 1;
    let (dar_n, dar_d) = profile.dar();
    let any_audio = sources.iter().any(|s| s.has_audio);

    // Stable identity for this project, derived from the trip rather than the clock.
    let doc_uuid = uuid_from(&format!("reel/doc/{title}"));
    let seq_uuid = uuid_from(&format!("reel/seq/{title}"));
    let doc_id = {
        // Kdenlive writes a millisecond timestamp here; any stable digits will do,
        // and stable digits mean a rebuild is byte-identical.
        let h = hex::encode(Sha1::digest(title.as_bytes()));
        let n = u64::from_str_radix(&h[0..12], 16).unwrap_or(1_700_000_000_000);
        format!("{}", 1_000_000_000_000 + (n % 900_000_000_000))
    };

    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    x.push_str(&format!(
        "<mlt LC_NUMERIC=\"C\" version=\"7.0.0\" producer=\"main_bin\" title=\"{}\" root=\"{}\">\n",
        xml_escape(title),
        xml_escape(&root.display().to_string())
    ));
    x.push_str(&format!(
        "  <profile description=\"{}\" width=\"{}\" height=\"{}\" progressive=\"1\" \
sample_aspect_num=\"{}\" sample_aspect_den=\"{}\" display_aspect_num=\"{}\" \
display_aspect_den=\"{}\" frame_rate_num=\"{}\" frame_rate_den=\"{}\" colorspace=\"709\"/>\n",
        xml_escape(&profile.describe()),
        profile.width,
        profile.height,
        profile.sar_num,
        profile.sar_den,
        dar_n,
        dar_d,
        profile.fps_num,
        profile.fps_den,
    ));

    // ---- bin clips: one per master, spanning the whole file ----
    for src in sources {
        let id = bin_id(&src.master);
        let len = src.length_f.max(1);
        let cu = uuid_from(&src.master.display().to_string());
        // Every mark on this master becomes a marker on its bin clip, at the
        // position *in the source*. The ruler guides say where a piece sits in the
        // edit; these say where it came from, and survive being re-cut elsewhere.
        let marks: Vec<(i64, String)> = segs
            .iter()
            .filter(|s| s.master == src.master)
            .map(|s| (s.in_f, s.guide()))
            .collect();

        x.push_str(&format!(
            "  <producer id=\"bin{id}\" in=\"0\" out=\"{}\">\n",
            len - 1
        ));
        prop(&mut x, "   ", "length", &len.to_string());
        prop(&mut x, "   ", "eof", "continue");
        prop(
            &mut x,
            "   ",
            "resource",
            &src.resource().display().to_string(),
        );
        prop(&mut x, "   ", "mlt_service", "avformat-novalidate");
        prop(&mut x, "   ", "seekable", "1");
        prop(
            &mut x,
            "   ",
            "audio_index",
            if src.has_audio { "0" } else { "-1" },
        );
        prop(&mut x, "   ", "video_index", "0");
        prop(&mut x, "   ", "mute_on_pause", "0");
        // Proxy wiring: `resource` is the proxy, the master rides along in
        // `originalurl`, and Kdenlive swaps back to it to render. Both paths are
        // absolute, so toggling "Proxy clips" off in the GUI still finds the master.
        if let Some(p) = &src.proxy {
            prop(
                &mut x,
                "   ",
                "kdenlive:originalurl",
                &src.master.display().to_string(),
            );
            prop(&mut x, "   ", "kdenlive:proxy", &p.display().to_string());
        }
        prop(&mut x, "   ", "kdenlive:control_uuid", &cu);
        prop(&mut x, "   ", "kdenlive:id", &id.to_string());
        prop(&mut x, "   ", "kdenlive:clip_type", "0");
        prop(&mut x, "   ", "kdenlive:clipname", &src.name);
        prop(&mut x, "   ", "kdenlive:duration", &len.to_string());
        prop(&mut x, "   ", "kdenlive:folderid", "-1");
        if !marks.is_empty() {
            prop(&mut x, "   ", "kdenlive:markers", &marker_json(&marks));
        }
        x.push_str("  </producer>\n");
    }

    // ---- background ----
    x.push_str(&format!(
        "  <producer id=\"black_track\" in=\"0\" out=\"{last}\">\n"
    ));
    prop(&mut x, "   ", "length", "2147483647");
    prop(&mut x, "   ", "eof", "continue");
    prop(&mut x, "   ", "resource", "black");
    prop(&mut x, "   ", "aspect_ratio", "1");
    prop(&mut x, "   ", "mlt_service", "color");
    prop(&mut x, "   ", "kdenlive:playlistid", "black_track");
    prop(&mut x, "   ", "mlt_image_format", "rgba");
    prop(&mut x, "   ", "set.test_audio", "0");
    x.push_str("  </producer>\n");

    // ---- timeline instances: a video half and (when there's sound) an audio half ----
    // Kdenlive models an A/V clip as two linked halves on two tracks; they share a
    // `kdenlive:id` back to the one bin clip.
    for src in sources {
        let id = bin_id(&src.master);
        let len = src.length_f.max(1);
        let cu = uuid_from(&src.master.display().to_string());
        let half = |x: &mut String, tag: &str, audio: bool| {
            x.push_str(&format!(
                "  <producer id=\"{tag}{id}\" in=\"0\" out=\"{}\">\n",
                len - 1
            ));
            prop(x, "   ", "length", &len.to_string());
            prop(x, "   ", "eof", "pause");
            prop(x, "   ", "resource", &src.resource().display().to_string());
            prop(x, "   ", "mlt_service", "avformat-novalidate");
            prop(x, "   ", "seekable", "1");
            prop(x, "   ", "audio_index", if audio { "0" } else { "-1" });
            prop(x, "   ", "video_index", if audio { "-1" } else { "0" });
            prop(x, "   ", "mute_on_pause", "0");
            if audio {
                prop(x, "   ", "set.test_image", "1");
            } else {
                prop(x, "   ", "set.test_audio", "1");
            }
            prop(x, "   ", "kdenlive:control_uuid", &cu);
            prop(x, "   ", "kdenlive:id", &id.to_string());
            x.push_str("  </producer>\n");
        };
        half(&mut x, "v", false);
        if src.has_audio {
            half(&mut x, "a", true);
        }
    }

    // ---- video track ----
    x.push_str("  <playlist id=\"playlist0\">\n");
    for s in segs {
        x.push_str(&format!(
            "   <entry producer=\"v{}\" in=\"{}\" out=\"{}\">\n",
            bin_id(&s.master),
            s.in_f,
            s.out_f
        ));
        prop(
            &mut x,
            "    ",
            "kdenlive:id",
            &bin_id(&s.master).to_string(),
        );
        x.push_str("   </entry>\n");
    }
    x.push_str("  </playlist>\n");
    x.push_str("  <playlist id=\"playlist1\"/>\n");
    x.push_str(&format!(
        "  <tractor id=\"tractor0\" in=\"0\" out=\"{last}\">\n"
    ));
    prop(&mut x, "   ", "kdenlive:track_name", "V1");
    prop(&mut x, "   ", "kdenlive:trackheight", "70");
    prop(&mut x, "   ", "kdenlive:timeline_active", "1");
    prop(&mut x, "   ", "kdenlive:collapsed", "0");
    x.push_str("   <track hide=\"audio\" producer=\"playlist0\"/>\n");
    x.push_str("   <track hide=\"audio\" producer=\"playlist1\"/>\n");
    x.push_str("  </tractor>\n");

    // ---- audio track, only when something actually has sound ----
    if any_audio {
        x.push_str("  <playlist id=\"playlist2\">\n");
        for s in segs {
            // A silent source gets a gap of the same length, never a skipped entry:
            // dropping one would slide every later audio clip earlier and quietly
            // desync the whole track from the picture.
            match source_of(&s.master) {
                Some(src) if src.has_audio => {
                    x.push_str(&format!(
                        "   <entry producer=\"a{}\" in=\"{}\" out=\"{}\">\n",
                        bin_id(&s.master),
                        s.in_f,
                        s.out_f
                    ));
                    prop(
                        &mut x,
                        "    ",
                        "kdenlive:id",
                        &bin_id(&s.master).to_string(),
                    );
                    x.push_str("   </entry>\n");
                }
                _ => x.push_str(&format!("   <blank length=\"{}\"/>\n", s.frames())),
            }
        }
        x.push_str("  </playlist>\n");
        x.push_str("  <playlist id=\"playlist3\"/>\n");
        x.push_str(&format!(
            "  <tractor id=\"tractor1\" in=\"0\" out=\"{last}\">\n"
        ));
        prop(&mut x, "   ", "kdenlive:audio_track", "1");
        prop(&mut x, "   ", "kdenlive:track_name", "A1");
        prop(&mut x, "   ", "kdenlive:trackheight", "70");
        prop(&mut x, "   ", "kdenlive:timeline_active", "1");
        prop(&mut x, "   ", "kdenlive:collapsed", "0");
        x.push_str("   <track hide=\"video\" producer=\"playlist2\"/>\n");
        x.push_str("   <track hide=\"video\" producer=\"playlist3\"/>\n");
        x.push_str("  </tractor>\n");
    }

    // ---- the sequence: what Kdenlive actually opens as "the timeline" ----
    let guides: Vec<(i64, String)> = {
        let mut v = Vec::new();
        let mut pos = 0i64;
        for s in segs {
            v.push((pos, s.guide()));
            pos += s.frames();
        }
        v
    };
    x.push_str(&format!(
        "  <tractor id=\"{seq_uuid}\" in=\"0\" out=\"{last}\">\n"
    ));
    prop(&mut x, "   ", "kdenlive:uuid", &seq_uuid);
    prop(&mut x, "   ", "kdenlive:clipname", title);
    prop(&mut x, "   ", "kdenlive:producer_type", "17");
    prop(&mut x, "   ", "kdenlive:id", "1");
    prop(&mut x, "   ", "kdenlive:duration", &total.to_string());
    prop(&mut x, "   ", "kdenlive:maxduration", &total.to_string());
    prop(
        &mut x,
        "   ",
        "kdenlive:sequenceproperties.documentuuid",
        &doc_uuid,
    );
    prop(
        &mut x,
        "   ",
        "kdenlive:sequenceproperties.hasAudio",
        if any_audio { "1" } else { "0" },
    );
    prop(&mut x, "   ", "kdenlive:sequenceproperties.hasVideo", "1");
    prop(
        &mut x,
        "   ",
        "kdenlive:sequenceproperties.tracksCount",
        if any_audio { "2" } else { "1" },
    );
    prop(
        &mut x,
        "   ",
        "kdenlive:sequenceproperties.activeTrack",
        "1",
    );
    prop(&mut x, "   ", "kdenlive:sequenceproperties.position", "0");
    prop(&mut x, "   ", "kdenlive:sequenceproperties.zoom", "8");
    // The marks on the ruler. This property, on this element — `docproperties.guides`
    // is a legacy upgrade target and writing there loses them silently.
    prop(
        &mut x,
        "   ",
        "kdenlive:sequenceproperties.guides",
        &marker_json(&guides),
    );
    prop(&mut x, "   ", "kdenlive:sequenceproperties.groups", "[]");
    prop(
        &mut x,
        "   ",
        "kdenlive:control_uuid",
        &uuid_from(&format!("reel/ctl/{title}")),
    );
    x.push_str("   <track producer=\"black_track\"/>\n");
    if any_audio {
        x.push_str("   <track producer=\"tractor1\"/>\n");
    }
    x.push_str("   <track producer=\"tractor0\"/>\n");
    // Kdenlive's own auto-added transitions: `mix` sums each audio track, `qtblend`
    // composites each video track. `internal_added=237` is the marker it uses to
    // recognise them as its own rather than user-placed.
    let video_b = if any_audio { 2 } else { 1 };
    if any_audio {
        x.push_str("   <transition id=\"transition0\">\n");
        prop(&mut x, "    ", "a_track", "0");
        prop(&mut x, "    ", "b_track", "1");
        prop(&mut x, "    ", "mlt_service", "mix");
        prop(&mut x, "    ", "sum", "1");
        prop(&mut x, "    ", "accepts_blanks", "1");
        prop(&mut x, "    ", "always_active", "1");
        prop(&mut x, "    ", "internal_added", "237");
        x.push_str("   </transition>\n");
    }
    x.push_str("   <transition id=\"transition1\">\n");
    prop(&mut x, "    ", "a_track", "0");
    prop(&mut x, "    ", "b_track", &video_b.to_string());
    prop(&mut x, "    ", "mlt_service", "qtblend");
    prop(&mut x, "    ", "kdenlive_id", "qtblend");
    prop(&mut x, "    ", "always_active", "1");
    prop(&mut x, "    ", "internal_added", "237");
    x.push_str("   </transition>\n");
    x.push_str("  </tractor>\n");

    // ---- the bin ----
    x.push_str("  <playlist id=\"main_bin\">\n");
    prop(&mut x, "   ", "kdenlive:docproperties.version", "1.1");
    prop(&mut x, "   ", "kdenlive:docproperties.patchversion", "1");
    prop(&mut x, "   ", "kdenlive:docproperties.uuid", &doc_uuid);
    prop(&mut x, "   ", "kdenlive:docproperties.documentid", &doc_id);
    // `kdenlive:docproperties.profile` **is** the project profile, and it takes a
    // resolvable *identifier* (`uhd_2160p_2398`) — never a description, never a
    // path. Both of the wrong answers here were measured, and both are nasty:
    //   - an unresolvable name → Kdenlive blocks on a modal at load. Conditional,
    //     too: harmless at 30/1, fatal at 24000/1001, so every synthetic 30 fps
    //     fixture passed while every real DJI trip would have hung.
    //   - the property omitted → loads fine and silently falls back to Kdenlive's
    //     default, rendering 1080x1920 footage as 720x576 PAL. Worse than the hang,
    //     because nothing tells you.
    // So it's emitted only when a real stock profile matches, and left out
    // otherwise — the caller reports that case rather than letting it pass quietly.
    if let Some(id) = profile_id {
        prop(&mut x, "   ", "kdenlive:docproperties.profile", id);
    }
    prop(
        &mut x,
        "   ",
        "kdenlive:docproperties.activetimeline",
        &seq_uuid,
    );
    prop(
        &mut x,
        "   ",
        "kdenlive:docproperties.opensequences",
        &seq_uuid,
    );
    prop(&mut x, "   ", "kdenlive:docproperties.audioChannels", "2");
    prop(
        &mut x,
        "   ",
        "kdenlive:docproperties.enableproxy",
        if sources.iter().any(|s| s.proxy.is_some()) {
            "1"
        } else {
            "0"
        },
    );
    prop(&mut x, "   ", "kdenlive:docproperties.generateproxy", "0");
    prop(
        &mut x,
        "   ",
        "kdenlive:docproperties.enableexternalproxy",
        "0",
    );
    prop(
        &mut x,
        "   ",
        "kdenlive:docproperties.generateimageproxy",
        "0",
    );
    prop(&mut x, "   ", "kdenlive:sequenceFolder", "-1");
    // `xml_retain` is what makes MLT serialize this otherwise-disconnected playlist.
    prop(&mut x, "   ", "xml_retain", "1");
    for src in sources {
        x.push_str(&format!(
            "   <entry producer=\"bin{}\" in=\"0\" out=\"{}\"/>\n",
            bin_id(&src.master),
            src.length_f.max(1) - 1
        ));
    }
    // The sequence is a bin item too — that's how it shows up in the Project Bin.
    x.push_str(&format!(
        "   <entry producer=\"{seq_uuid}\" in=\"0\" out=\"{last}\"/>\n"
    ));
    x.push_str("  </playlist>\n");

    // ---- project tractor, last ----
    x.push_str(&format!(
        "  <tractor id=\"projecttractor\" in=\"0\" out=\"{last}\">\n"
    ));
    prop(&mut x, "   ", "kdenlive:projectTractor", "1");
    x.push_str(&format!(
        "   <track producer=\"{seq_uuid}\" in=\"0\" out=\"{last}\"/>\n"
    ));
    x.push_str("  </tractor>\n");
    x.push_str("</mlt>\n");
    x
}

// ---- assembly ----

/// Order marks the way the trip happened: by the master's capture time, then by
/// position within it. `marks.tsv` is in the order the UI last wrote it, which is
/// close but not guaranteed, and a timeline that jumps around in time reads as a
/// bug even when every segment is right.
pub fn order_marks(marks: &mut [Mark], captured: &HashMap<PathBuf, i64>) {
    marks.sort_by(|a, b| {
        let ka = captured.get(Path::new(&a.master)).copied().unwrap_or(0);
        let kb = captured.get(Path::new(&b.master)).copied().unwrap_or(0);
        ka.cmp(&kb)
            .then_with(|| a.master.cmp(&b.master))
            .then_with(|| {
                a.start
                    .partial_cmp(&b.start)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
}

/// Turn a resolved mark into a segment against a known profile and source length.
pub fn segment_of(
    master: &Path,
    label: &str,
    start: f64,
    end: f64,
    fps: f64,
    length_f: i64,
) -> Segment {
    let name = master
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("clip")
        .to_string();
    let last = (length_f - 1).max(0);
    let in_f = frame_at(start, fps).min(last);
    // `end` is exclusive in seconds; `out` is inclusive in frames.
    let out_f = (frame_at(end, fps) - 1).clamp(in_f, last);
    Segment {
        master: master.to_path_buf(),
        name,
        label: label.to_string(),
        in_f,
        out_f,
    }
}

/// Build `<trip>/<trip>.kdenlive` from the trip's marks and return what landed in
/// it. `Err` only when there's nothing to build from — no trip, no marks, or (the
/// archived case) no master still on disk to point at.
pub fn build_timeline(cfg: &Config, trip: &str) -> Result<TimelineResult, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let dir = cfg.lib.join(trip);
    if !dir.join(".reel").is_file() {
        return Err(format!("no such trip: {trip}"));
    }
    let mut marks = read_marks(&dir);
    if marks.is_empty() {
        return Err(format!("no marks in '{trip}' — review it first"));
    }

    // Resolve each mark's master once, reusing `cut`'s rename-recovery so a moved
    // trip builds a timeline exactly as well as it cuts.
    let mut located: HashMap<String, Option<PathBuf>> = HashMap::new();
    for m in &marks {
        located
            .entry(m.master.clone())
            .or_insert_with(|| crate::cut::locate(Path::new(&m.master), &dir));
    }
    let captured: HashMap<PathBuf, i64> = located
        .values()
        .flatten()
        .map(|p| (p.clone(), captured_at(p)))
        .collect();
    // Order by the *resolved* path's capture time, but sort the marks as read.
    let mut keyed: HashMap<PathBuf, i64> = HashMap::new();
    for m in &marks {
        if let Some(Some(p)) = located.get(&m.master) {
            keyed.insert(PathBuf::from(&m.master), *captured.get(p).unwrap_or(&0));
        }
    }
    order_marks(&mut marks, &keyed);

    // Probe every distinct master that resolved. One ffprobe each, headers only.
    let mut probes: HashMap<PathBuf, ProbeInfo> = HashMap::new();
    for p in located.values().flatten() {
        if !probes.contains_key(p) {
            if let Some(info) = probe(p) {
                probes.insert(p.clone(), info);
            }
        }
    }

    // Profile from the marked footage, weighted by how many marks each master owns —
    // the format most of the *timeline* is in, not most of the folder.
    let mut weights: Vec<Profile> = Vec::new();
    for m in &marks {
        if let Some(Some(p)) = located.get(&m.master) {
            if let Some(info) = probes.get(p) {
                weights.push(info.profile.clone());
            }
        }
    }
    // The format most of the marked footage is in, then conformed to something
    // Kdenlive will open. Every frame number below is computed against the result,
    // so a conformed rate moves the whole timeline consistently rather than
    // sliding clips against each other.
    let picked = choose_profile(&weights);
    let real_fps = picked.describe();
    let (profile, profile_id, conformed) = conform(&picked);
    let fps = profile.fps();

    let mut segs: Vec<Segment> = Vec::new();
    let mut skipped = 0usize;
    for m in &marks {
        let Some(Some(path)) = located.get(&m.master) else {
            skipped += 1;
            continue;
        };
        let Some(info) = probes.get(path) else {
            skipped += 1;
            continue;
        };
        let length_f = frame_at(info.duration, fps).max(1);
        let seg = segment_of(path, &m.label, m.start, m.end, fps, length_f);
        if seg.frames() > 0 {
            segs.push(seg);
        } else {
            skipped += 1;
        }
    }
    if segs.is_empty() {
        return Err(format!(
            "no footage behind the marks in '{trip}' — the raw is archived or missing, \
             so there's nothing to build a timeline from. Restore it first."
        ));
    }

    // The bin, in first-use order. Each carries the master's *full* length, so an
    // edge stays draggable past the mark once you're in Kdenlive.
    let mut sources: Vec<Source> = Vec::new();
    for s in &segs {
        if sources.iter().any(|q| q.master == s.master) {
            continue;
        }
        let info = probes.get(&s.master);
        // The proxy reel built while you were reviewing, if it's still there. Built
        // with no `-r`/`-vsync`, so it carries the master's frame count and rate
        // exactly and stands in frame-for-frame — the only reason it's safe to hand
        // Kdenlive as a proxy at all. It's also all-intra, so the timeline scrubs on
        // it rather than walking a GOP per seek.
        let proxy = dir
            .join(".proxies")
            .join(format!("{}.mp4", crate::media::rel_stem(&s.master, &dir)));
        sources.push(Source {
            master: s.master.clone(),
            name: s.name.clone(),
            length_f: info
                .map(|i| frame_at(i.duration, fps).max(1))
                .unwrap_or(s.out_f + 1)
                .max(s.out_f + 1),
            has_audio: info.map(|i| i.has_audio).unwrap_or(false),
            proxy: proxy.is_file().then_some(proxy),
        });
    }

    let title = format!("reel: {trip}");
    let xml = timeline_xml(
        &title,
        &dir,
        &profile,
        profile_id.as_deref(),
        &sources,
        &segs,
    );
    let out = dir.join(format!("{trip}.kdenlive"));
    let tmp = crate::store::temp_sibling(&out);
    std::fs::write(&tmp, xml).map_err(|e| format!("couldn't write the timeline: {e}"))?;
    std::fs::rename(&tmp, &out).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("couldn't write the timeline: {e}")
    })?;

    let frames: i64 = segs.iter().map(|s| s.frames()).sum();
    let sources = {
        let mut v: Vec<&PathBuf> = segs.iter().map(|s| &s.master).collect();
        v.sort();
        v.dedup();
        v.len()
    };
    Ok(TimelineResult {
        trip: trip.to_string(),
        path: out.display().to_string(),
        segments: segs.len(),
        sources,
        skipped,
        duration: frames as f64 / fps,
        profile: profile.describe(),
        profile_id,
        // Only reported when the rate actually moved, so the UI stays quiet in the
        // common case and says something only when the footage was conformed.
        conformed_from: conformed.then_some(real_fps),
    })
}
