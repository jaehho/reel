//! Per-trip sharing via the Nextcloud OCS Share API.
//!
//! reel moves footage with rclone, but *who may see* a trip's cloud folder is a
//! Nextcloud access-control decision rclone can't make. This module shares a
//! single trip's cloud folder (`<cloud>/<trip>`) with named Nextcloud users and
//! revokes them, so a friend only sees the trips they're part of instead of the
//! whole cloud.
//!
//! No second login: the credentials come from the rclone webdav remote that
//! already backs the cloud. `rclone config dump` yields its `url`/`user`/`pass`,
//! `rclone reveal` de-obscures the password, and the Nextcloud base URL is the
//! part of the webdav URL before `/remote.php`. Requests go out through `curl`
//! (its Basic-auth fed on stdin, never argv, so no secret hits the process
//! table) — the same shell-out approach as the rest of the engine.

use crate::config::Config;
use crate::model::{Sharee, TripShare};
use crate::rclone::remote_name;
use crate::store::{now_epoch, write_atomic};
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

/// OCS collaborator permissions: read+update+create+delete — everything but
/// reshare, so a friend can pull the trip and push their own footage back, but
/// can't hand the cloud folder on to others. (1 read | 2 update | 4 create |
/// 8 delete = 15.)
const COLLAB_PERMS: u32 = 15;

/// A resolved connection to the cloud's Nextcloud, derived entirely from the
/// rclone remote backing `REEL_REMOTE`.
struct Ocs {
    /// Nextcloud base URL, e.g. `https://cloud.example.com` (no trailing slash).
    base: String,
    user: String,
    /// Plaintext password / app-password, de-obscured from the rclone config.
    pass: String,
    /// The cloud's subpath within the user's files, e.g. `Reels` (may be empty).
    root: String,
    /// Mirror the rclone remote's `no_check_certificate`: skip TLS verification
    /// only when the remote itself is set to (e.g. a homelab self-signed cert), so
    /// reel and rclone trust exactly the same things — never more.
    insecure: bool,
}

/// rclone serializes booleans as the strings `"true"`/`"false"` in `config dump`;
/// accept either a JSON bool or those strings.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Nextcloud webdav URLs look like `https://host[/subdir]/remote.php/dav/files/<user>/`.
/// The OCS API lives at the same origin (and subdir), so the base is everything
/// before `/remote.php`.
fn base_from_webdav(url: &str) -> Option<String> {
    let cut = url.find("/remote.php")?;
    let base = url[..cut].trim_end_matches('/');
    (!base.is_empty()).then(|| base.to_string())
}

/// The cloud's path within the Nextcloud user's files: the part of `REEL_REMOTE`
/// after the remote name. `nextcloud:Reels` → `Reels`; `nextcloud:` → ``.
fn cloud_root(remote: &str) -> String {
    remote
        .split_once(':')
        .map(|(_, p)| p)
        .unwrap_or("")
        .trim_matches('/')
        .to_string()
}

/// A trip's cloud folder as an absolute OCS `path`, within the user's files.
fn trip_path(root: &str, trip: &str) -> String {
    if root.is_empty() {
        format!("/{trip}")
    } else {
        format!("/{root}/{trip}")
    }
}

/// The configured remote's entry from `rclone config dump`, insisting it's a
/// webdav remote (the only kind we can drive the OCS API against).
fn dump_remote(cfg: &Config) -> Result<Value, String> {
    let name = remote_name(&cfg.remote)
        .ok_or("sharing needs a Nextcloud remote — this cloud is a local path")?;
    let out = Command::new("rclone")
        .args(["config", "dump"])
        .output()
        .map_err(|e| format!("couldn't run rclone: {e}"))?;
    if !out.status.success() {
        return Err("couldn't read rclone config".into());
    }
    let all: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("bad rclone config: {e}"))?;
    let remote = all
        .get(name)
        .cloned()
        .ok_or_else(|| format!("rclone remote '{name}:' not found"))?;
    let typ = remote.get("type").and_then(Value::as_str).unwrap_or("");
    if typ != "webdav" {
        return Err(format!(
            "sharing needs a Nextcloud (webdav) remote; '{name}:' is a {typ} remote"
        ));
    }
    Ok(remote)
}

/// Confirm curl is available — it's how we reach the OCS API.
fn which_curl() -> Result<(), String> {
    Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "curl isn't installed — needed to talk to the Nextcloud share API".to_string())
        .and_then(|s| {
            s.success()
                .then_some(())
                .ok_or_else(|| "curl failed to run".to_string())
        })
}

/// Whether per-trip sharing can run: the cloud must be a Nextcloud webdav remote
/// with a saved user, curl must be installed, and the URL must look like
/// Nextcloud. Cheap — doesn't de-obscure the password, so the UI can call it on
/// panel open to decide whether to offer sharing at all.
pub fn sharing_available(cfg: &Config) -> Result<(), String> {
    let remote = dump_remote(cfg)?;
    let url = remote.get("url").and_then(Value::as_str).unwrap_or("");
    base_from_webdav(url).ok_or("the cloud's webdav URL isn't a Nextcloud remote.php URL")?;
    if remote
        .get("user")
        .and_then(Value::as_str)
        .unwrap_or("")
        .is_empty()
    {
        return Err("the rclone remote has no user — can't authenticate to Nextcloud".into());
    }
    which_curl()
}

/// Fully resolve the OCS connection, de-obscuring the password. Called by every
/// mutating op just before it needs to authenticate.
fn resolve(cfg: &Config) -> Result<Ocs, String> {
    let remote = dump_remote(cfg)?;
    let url = remote.get("url").and_then(Value::as_str).unwrap_or("");
    let base =
        base_from_webdav(url).ok_or("the cloud's webdav URL isn't a Nextcloud remote.php URL")?;
    let user = remote
        .get("user")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if user.is_empty() {
        return Err("the rclone remote has no user — can't authenticate to Nextcloud".into());
    }
    let obscured = remote.get("pass").and_then(Value::as_str).unwrap_or("");
    if obscured.is_empty() {
        return Err(
            "the rclone remote has no saved password — can't authenticate to Nextcloud".into(),
        );
    }
    let pass = reveal(obscured)?;
    which_curl()?;
    Ok(Ocs {
        base,
        user,
        pass,
        root: cloud_root(&cfg.remote),
        insecure: truthy(remote.get("no_check_certificate")),
    })
}

/// De-obscure an rclone-stored password (`rclone reveal`), the reverse of the
/// obscuring rclone applies in its config. The obscured form (already on disk) is
/// what's passed as the argument; the plaintext only ever lives in memory and is
/// handed to curl on stdin.
fn reveal(obscured: &str) -> Result<String, String> {
    let out = Command::new("rclone")
        .args(["reveal", obscured])
        .output()
        .map_err(|e| format!("couldn't run rclone: {e}"))?;
    if !out.status.success() {
        return Err("couldn't read the remote's password from rclone".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\n', '\r'])
        .to_string())
}

/// Escape a value for a curl `-K` config double-quoted string.
fn curl_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], "")
}

/// One OCS Share API call. `method` is GET/POST/DELETE, `endpoint` is appended to
/// the shares API path, and `params` are url-encoded (the query string for GET,
/// the form body otherwise). Basic-auth is fed to curl on stdin so `user:pass`
/// never appears in the process list. Returns the `ocs.data` payload when
/// `ocs.meta.status == "ok"`, else the server's message.
fn call(ocs: &Ocs, method: &str, endpoint: &str, params: &[(&str, &str)]) -> Result<Value, String> {
    let url = format!(
        "{}/ocs/v2.php/apps/files_sharing/api/v1/{}",
        ocs.base, endpoint
    );
    let mut args: Vec<String> = vec![
        "-sS".into(),
        "-X".into(),
        method.into(),
        "-H".into(),
        "OCS-APIRequest: true".into(),
        "-H".into(),
        "Accept: application/json".into(),
        // credentials come from the config on stdin, keeping them out of argv
        "-K".into(),
        "-".into(),
    ];
    if ocs.insecure {
        args.push("-k".into()); // remote opted out of cert checks; match it
    }
    if method == "GET" {
        args.push("-G".into()); // send params as the url-encoded query string
    }
    for (k, v) in params {
        args.push("--data-urlencode".into());
        args.push(format!("{k}={v}"));
    }
    args.push(url);

    let mut child = Command::new("curl")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    let creds = format!(
        "user = \"{}\"\n",
        curl_escape(&format!("{}:{}", ocs.user, ocs.pass))
    );
    child
        .stdin
        .take()
        .ok_or("couldn't pass credentials to curl")?
        .write_all(creds.as_bytes())
        .map_err(|e| format!("couldn't pass credentials to curl: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl failed: {e}"))?;
    // A transport failure (bad cert, no host, refused) leaves no body to parse.
    // curl's first stderr line is the real cause (`curl: (60) SSL: …`); the tail
    // is just its "visit the web page above" epilogue, so lead with the first.
    if out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("no response");
        return Err(format!("couldn't reach Nextcloud — {msg}"));
    }
    let body: Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| "Nextcloud returned an unexpected (non-JSON) response".to_string())?;
    ocs_data(&body)
}

/// Unwrap an OCS v2 envelope (`{ocs:{meta:{status,statuscode,message},data}}`):
/// `data` on success, else the server's message and code.
fn ocs_data(body: &Value) -> Result<Value, String> {
    let meta = &body["ocs"]["meta"];
    if meta["status"].as_str() == Some("ok") {
        Ok(body["ocs"]["data"].clone())
    } else {
        let msg = meta["message"].as_str().unwrap_or("share request failed");
        let code = meta["statuscode"].as_i64().unwrap_or(0);
        Err(format!("Nextcloud: {msg} ({code})"))
    }
}

/// Read an OCS id that may arrive as a JSON number or a string.
fn id_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// One OCS share object → `TripShare`, keeping only user shares (share_type 0);
/// link/group/email shares are ignored for the friends list.
fn one_share(v: &Value) -> Option<TripShare> {
    if v["share_type"].as_i64().unwrap_or(-1) != 0 {
        return None;
    }
    let id = id_str(&v["id"])?;
    let user = v["share_with"].as_str()?.to_string();
    let display = v["share_with_displayname"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&user)
        .to_string();
    Some(TripShare {
        id,
        user,
        display_name: display,
        permissions: v["permissions"].as_u64().unwrap_or(0) as u32,
    })
}

/// The user shares from a `GET /shares` payload, sorted by display name.
fn parse_shares(data: &Value) -> Vec<TripShare> {
    let mut out: Vec<TripShare> = data
        .as_array()
        .map(|a| a.iter().filter_map(one_share).collect())
        .unwrap_or_default();
    out.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });
    out
}

/// The user matches from a `GET /sharees` payload (`exact.users` then `users`),
/// de-duplicated and excluding yourself.
fn parse_sharees(data: &Value, me: &str) -> Vec<Sharee> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let groups = [data["exact"]["users"].as_array(), data["users"].as_array()];
    for arr in groups.into_iter().flatten() {
        for m in arr {
            let Some(user) = m["value"]["shareWith"].as_str() else {
                continue;
            };
            if user == me || !seen.insert(user.to_string()) {
                continue;
            }
            let display = m["label"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(user)
                .to_string();
            out.push(Sharee {
                user: user.to_string(),
                display_name: display,
            });
        }
    }
    out
}

// ---- share cache (network-free "shared with N" for the dashboard chip) ----
// Every successful OCS op writes the trip's share list here (`id\tuser\tdisplay`
// rows under a `#checked` header), so `list_trips` can show a per-trip share
// count without a network hit — the same lazy-cache idea as the cloud listing.

/// (id, user, display) rows from one share-cache file.
fn read_share_rows(path: &std::path::Path) -> Vec<[String; 3]> {
    let mut rows = Vec::new();
    if let Ok(txt) = std::fs::read_to_string(path) {
        for line in txt.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut f = line.split('\t');
            let (Some(id), Some(user)) = (f.next(), f.next()) else {
                continue;
            };
            let disp = f.next().unwrap_or(user);
            rows.push([id.to_string(), user.to_string(), disp.to_string()]);
        }
    }
    rows
}

/// (id, user, display) rows of a trip's cached share list.
fn load_share_rows(cfg: &Config, trip: &str) -> Vec<[String; 3]> {
    read_share_rows(&cfg.share_cache_path(trip))
}

/// Persist a trip's share rows atomically. Best-effort: a cache write failure
/// never fails the share op it rides on — the chip just stays stale until the
/// next fetch.
fn save_share_rows(cfg: &Config, trip: &str, rows: &[[String; 3]]) {
    let path = cfg.share_cache_path(trip);
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let mut s = format!("#checked\t{}\n", now_epoch());
    for r in rows {
        s.push_str(&format!("{}\t{}\t{}\n", r[0], r[1], r[2]));
    }
    let _ = write_atomic(&path, &s);
}

/// One share as a cache row, scrubbing the tab/newline delimiters out of the
/// (user-controlled) names so a display name can't corrupt the TSV.
fn share_row(s: &TripShare) -> [String; 3] {
    let scrub = |x: &str| x.replace(['\t', '\n', '\r'], " ");
    [s.id.clone(), scrub(&s.user), scrub(&s.display_name)]
}

/// How many people a trip's cloud folder was shared with as of the last fetch —
/// read straight from the cache, no network. `None` = never fetched (or the trip
/// isn't in the cloud), so the card shows no chip; `Some(0)` = in the cloud but
/// shared with nobody yet (a "Share…" quick action); `Some(n)` = shared with n.
/// The cache *file* existing is the "it's shareable" signal — a successful list
/// (even of zero shares) only happens when the trip's cloud folder is really there.
pub fn cached_shares(cfg: &Config, trip: &str) -> Option<usize> {
    let txt = std::fs::read_to_string(cfg.share_cache_path(trip)).ok()?;
    Some(
        txt.lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .count(),
    )
}

/// Everyone you've shared *any* trip with — the union of all per-trip share
/// caches, deduped by username and sorted by display name. A network-free
/// "friends" list to seed the add-a-friend dropdown; the panel drops whoever's
/// already on the trip in hand. Empty until you've shared at least one trip.
pub fn known_sharees(cfg: &Config) -> Vec<Sharee> {
    let mut by_user: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(cfg.share_cache_dir()) {
        for e in entries.flatten() {
            // Only real caches — never a temp sibling a concurrent write left behind.
            if e.path().extension().and_then(|x| x.to_str()) != Some("tsv") {
                continue;
            }
            for r in read_share_rows(&e.path()) {
                let [_, user, display] = r;
                by_user.entry(user).or_insert(display);
            }
        }
    }
    let mut out: Vec<Sharee> = by_user
        .into_iter()
        .map(|(user, display_name)| Sharee { user, display_name })
        .collect();
    out.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });
    out
}

/// Everyone a trip's cloud folder is currently shared with (user shares only).
pub fn list_shares(cfg: &Config, trip: &str) -> Result<Vec<TripShare>, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let ocs = resolve(cfg)?;
    let path = trip_path(&ocs.root, trip);
    let data = call(
        &ocs,
        "GET",
        "shares",
        &[("path", &path), ("reshares", "false")],
    )?;
    let shares = parse_shares(&data);
    save_share_rows(cfg, trip, &shares.iter().map(share_row).collect::<Vec<_>>());
    Ok(shares)
}

/// Share a trip's cloud folder with a Nextcloud user, as a collaborator.
pub fn add_share(cfg: &Config, trip: &str, user: &str) -> Result<TripShare, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let user = user.trim();
    if user.is_empty() {
        return Err("no username to share with".into());
    }
    let ocs = resolve(cfg)?;
    if user == ocs.user {
        return Err("that's your own account — it already owns the trip".into());
    }
    let path = trip_path(&ocs.root, trip);
    let perms = COLLAB_PERMS.to_string();
    let data = call(
        &ocs,
        "POST",
        "shares",
        &[
            ("path", &path),
            ("shareType", "0"),
            ("shareWith", user),
            ("permissions", &perms),
        ],
    )?;
    let share = one_share(&data)
        .ok_or_else(|| "shared, but Nextcloud returned no share record".to_string())?;
    // Keep the dashboard chip's cache exact without a re-list: upsert this person.
    let mut rows = load_share_rows(cfg, trip);
    rows.retain(|r| r[0] != share.id);
    rows.push(share_row(&share));
    save_share_rows(cfg, trip, &rows);
    Ok(share)
}

/// Revoke a share by its OCS id. `trip` is only used to prune the network-free
/// share cache so the card chip updates without a re-list.
pub fn remove_share(cfg: &Config, trip: &str, share_id: &str) -> Result<(), String> {
    let id = share_id.trim();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid share id: {share_id:?}"));
    }
    let ocs = resolve(cfg)?;
    call(&ocs, "DELETE", &format!("shares/{id}"), &[])?;
    if valid_trip(trip) {
        let mut rows = load_share_rows(cfg, trip);
        rows.retain(|r| r[0] != id);
        save_share_rows(cfg, trip, &rows);
    }
    Ok(())
}

/// Autocomplete candidate recipients for the "add friend" box.
pub fn search_sharees(cfg: &Config, query: &str) -> Result<Vec<Sharee>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let ocs = resolve(cfg)?;
    let data = call(
        &ocs,
        "GET",
        "sharees",
        &[
            ("search", q),
            ("itemType", "folder"),
            ("shareType", "0"),
            ("perPage", "25"),
            ("lookup", "false"),
        ],
    )?;
    Ok(parse_sharees(&data, &ocs.user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base_url_trims_webdav_suffix() {
        assert_eq!(
            base_from_webdav("https://nc.example.net/remote.php/dav/files/jaeho/").as_deref(),
            Some("https://nc.example.net")
        );
        // Nextcloud served from a subdirectory keeps the subdir.
        assert_eq!(
            base_from_webdav("https://host/nextcloud/remote.php/webdav/").as_deref(),
            Some("https://host/nextcloud")
        );
        assert_eq!(base_from_webdav("https://host/dav/files/x/"), None);
    }

    #[test]
    fn cloud_root_and_trip_path() {
        assert_eq!(cloud_root("nextcloud:Reels"), "Reels");
        assert_eq!(cloud_root("nextcloud:Reels/"), "Reels");
        assert_eq!(cloud_root("nextcloud:"), "");
        assert_eq!(trip_path("Reels", "Japan 2024"), "/Reels/Japan 2024");
        assert_eq!(trip_path("", "Japan"), "/Japan");
    }

    #[test]
    fn curl_escape_quotes_and_backslashes() {
        assert_eq!(curl_escape(r#"jaeho:pa"ss\word"#), r#"jaeho:pa\"ss\\word"#);
        assert_eq!(curl_escape("a\nb\rc"), "abc");
    }

    #[test]
    fn truthy_accepts_rclone_string_and_json_bool() {
        assert!(truthy(Some(&json!("true"))));
        assert!(truthy(Some(&json!("True"))));
        assert!(truthy(Some(&json!(true))));
        assert!(!truthy(Some(&json!("false"))));
        assert!(!truthy(Some(&json!("")))); // unset-ish
        assert!(!truthy(None));
    }

    #[test]
    fn ocs_envelope_ok_and_failure() {
        let ok = json!({"ocs": {"meta": {"status": "ok", "statuscode": 200}, "data": [1, 2]}});
        assert_eq!(ocs_data(&ok).unwrap(), json!([1, 2]));
        let bad = json!({"ocs": {"meta": {"status": "failure", "statuscode": 404, "message": "not found"}, "data": []}});
        assert!(ocs_data(&bad).unwrap_err().contains("not found"));
    }

    #[test]
    fn parses_user_shares_ignoring_links_and_id_shape() {
        // A user share (id as number), a public link (id as string) — only the
        // user share survives, and a missing display name falls back to the id.
        let data = json!([
            {"id": 42, "share_type": 0, "share_with": "alice", "share_with_displayname": "Alice A", "permissions": 15},
            {"id": "7", "share_type": 3, "share_with": null, "permissions": 1},
            {"id": 9, "share_type": 0, "share_with": "bob", "share_with_displayname": "", "permissions": 1},
        ]);
        let shares = parse_shares(&data);
        assert_eq!(shares.len(), 2);
        // sorted by display name: "Alice A" before "bob"
        assert_eq!(shares[0].user, "alice");
        assert_eq!(shares[0].id, "42");
        assert_eq!(shares[0].permissions, 15);
        assert_eq!(shares[1].user, "bob");
        assert_eq!(shares[1].display_name, "bob"); // fell back to the id
    }

    fn test_cfg(state: &std::path::Path) -> Config {
        Config {
            lib: state.join("lib"),
            remote: "nextcloud:Reels".into(),
            user: "jaeho".into(),
            state_dir: state.to_path_buf(),
            cache_dir: state.join("cache"),
            session_gap: 21600,
            dji_sd: None,
            gopro_sd: None,
            media_user: "jaeho".into(),
        }
    }

    #[test]
    fn share_cache_round_trips_and_scrubs() {
        let d = tempfile::tempdir().unwrap();
        let cfg = test_cfg(d.path());
        // Never fetched → None (no chip), distinct from a checked-but-empty trip.
        assert_eq!(cached_shares(&cfg, "DOHA"), None);
        // A display name carrying the TSV delimiter mustn't corrupt the row.
        let shares = [
            TripShare {
                id: "42".into(),
                user: "alice".into(),
                display_name: "Alice\tA".into(),
                permissions: 15,
            },
            TripShare {
                id: "9".into(),
                user: "bob".into(),
                display_name: "Bob".into(),
                permissions: 15,
            },
        ];
        save_share_rows(
            &cfg,
            "DOHA",
            &shares.iter().map(share_row).collect::<Vec<_>>(),
        );
        assert_eq!(cached_shares(&cfg, "DOHA"), Some(2));
        let rows = load_share_rows(&cfg, "DOHA");
        assert_eq!(rows.iter().find(|r| r[0] == "42").unwrap()[2], "Alice A");
        // Another trip's cache is independent (still never fetched).
        assert_eq!(cached_shares(&cfg, "KYOTO"), None);
        // Pruning to empty leaves a valid (header-only) cache → Some(0): the trip
        // is shareable (folder listed), just shared with nobody now.
        save_share_rows(&cfg, "DOHA", &[]);
        assert_eq!(cached_shares(&cfg, "DOHA"), Some(0));
    }

    #[test]
    fn known_sharees_unions_and_dedups_across_trips() {
        let d = tempfile::tempdir().unwrap();
        let cfg = test_cfg(d.path());
        assert!(known_sharees(&cfg).is_empty()); // nothing shared yet
        let mk = |id: &str, user: &str, disp: &str| TripShare {
            id: id.into(),
            user: user.into(),
            display_name: disp.into(),
            permissions: 15,
        };
        let rows = |v: &[TripShare]| v.iter().map(share_row).collect::<Vec<_>>();
        save_share_rows(
            &cfg,
            "DOHA",
            &rows(&[mk("1", "bob", "Bob"), mk("2", "alice", "Alice A")]),
        );
        save_share_rows(&cfg, "KYOTO", &rows(&[mk("3", "alice", "Alice A")])); // alice again
                                                                               // deduped by user, sorted by display name → "Alice A", then "Bob".
        let friends = known_sharees(&cfg);
        let users: Vec<&str> = friends.iter().map(|s| s.user.as_str()).collect();
        assert_eq!(users, ["alice", "bob"]);
    }

    #[test]
    fn parses_sharees_dedups_and_drops_self() {
        let data = json!({
            "exact": {"users": [{"label": "Alice", "value": {"shareType": 0, "shareWith": "alice"}}]},
            "users": [
                {"label": "Alice A", "value": {"shareType": 0, "shareWith": "alice"}},
                {"label": "Me", "value": {"shareType": 0, "shareWith": "jaeho"}},
                {"label": "Bob", "value": {"shareType": 0, "shareWith": "bob"}}
            ]
        });
        let sharees = parse_sharees(&data, "jaeho");
        let users: Vec<&str> = sharees.iter().map(|s| s.user.as_str()).collect();
        assert_eq!(users, ["alice", "bob"]); // exact first, alice de-duped, self dropped
    }
}
