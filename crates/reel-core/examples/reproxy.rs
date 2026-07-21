//! Rebuild the proxies that are already cached, on today's recipe.
//! `cargo run -p reel-core --example reproxy` (or `make reproxy`)
//!
//! `ensure_proxy` returns a cache hit without looking inside the file, so changing
//! the ffmpeg recipe in `proxy.rs` leaves everything already on disk built the old
//! way — playable, and stale in a way nothing surfaces. This walks the cache and
//! rebuilds it through `ensure_proxy` itself, so what lands is whatever `proxy.rs`
//! builds today rather than a copy of the recipe here that can drift from it.
//!
//! Only refreshes what's already cached. It never proxies a clip you haven't
//! reviewed, so it can't balloon the cache beyond the size it already was.

use std::collections::HashSet;
use std::io::Write;
use std::time::Instant;

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn main() {
    let cfg = reel_core::Config::from_env();
    let started = Instant::now();
    let (mut rebuilt, mut failed, mut orphans) = (0usize, 0usize, 0usize);
    let (mut was, mut now) = (0u64, 0u64);

    for dir in reel_core::trips::trip_dirs(&cfg) {
        // The name `ensure_proxy` wants is the path under the library, which for a
        // nested trip is `parent/child` — `trip_dirs` finds those at depth 2.
        let Some(trip) = dir.strip_prefix(&cfg.lib).ok().and_then(|p| p.to_str()) else {
            continue;
        };
        let pdir = dir.join(".proxies");
        if !pdir.is_dir() {
            continue;
        }

        // What's cached, by proxy filename. Masters are matched *to* this rather than
        // parsed back out of it: `rel_stem` flattens `/` to `__`, and reversing that
        // would guess wrong on any folder with a `__` in its name.
        let cached: HashSet<String> = std::fs::read_dir(&pdir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "mp4"))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        if cached.is_empty() {
            continue;
        }

        // No masters at all is a different thing from "these particular masters went
        // away" — an archived trip, or a library that isn't fully mounted. Nothing
        // here can be rebuilt, and treating the whole cache as orphaned would delete
        // it on the strength of a guess. Leave it alone.
        let masters = reel_core::media::masters_in(&dir);
        if masters.is_empty() {
            println!("  {trip}: {} cached, no masters — skipped", cached.len());
            continue;
        }

        let mut hit: HashSet<String> = HashSet::new();
        for master in masters {
            let name = format!("{}.mp4", reel_core::media::rel_stem(&master, &dir));
            if !cached.contains(&name) {
                continue;
            }
            hit.insert(name.clone());
            let path = pdir.join(&name);
            let before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

            // Delete first: `ensure_proxy` short-circuits on any existing file, so a
            // rebuild is only a rebuild once the old one is out of the way.
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("  !! {name}: couldn't clear the old proxy: {e}");
                failed += 1;
                continue;
            }
            let t = Instant::now();
            match reel_core::ensure_proxy(&cfg, trip, &master) {
                Ok(p) => {
                    let after = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                    was += before;
                    now += after;
                    rebuilt += 1;
                    println!(
                        "  {trip}/{name}  {:.0} MB -> {:.0} MB  {:.1}s",
                        mb(before),
                        mb(after),
                        t.elapsed().as_secs_f64()
                    );
                }
                // The old file is already gone, which is fine — a proxy is a cache,
                // and review rebuilds whatever's missing the next time it's opened.
                Err(e) => {
                    eprintln!("  !! {trip}/{name}: {e}");
                    failed += 1;
                }
            }
            let _ = std::io::stdout().flush();
        }

        // Cached under a name no master claims: the master was renamed, moved, or
        // removed since. Nothing can rebuild these, and they'd sit there forever.
        for name in cached.difference(&hit) {
            match std::fs::remove_file(pdir.join(name)) {
                Ok(()) => {
                    orphans += 1;
                    println!("  {trip}/{name}  dropped (no master)");
                }
                Err(e) => eprintln!("  !! {trip}/{name}: {e}"),
            }
        }
    }

    println!(
        "\n{rebuilt} rebuilt, {failed} failed, {orphans} orphaned · \
         {:.1} GB -> {:.1} GB · {:.0}s",
        mb(was) / 1024.0,
        mb(now) / 1024.0,
        started.elapsed().as_secs_f64()
    );
}
