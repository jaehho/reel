//! Build a trip's Kdenlive timeline and print what landed in it.
//!
//! `cargo run -p reel-core --example timeline -- <trip>` — the debugging twin of
//! `make dump`, for checking the export against real footage without launching the
//! app or the editor. Honours `REEL_LIB`, so it can be pointed at a scratch library
//! whose `marks.tsv` references masters that live somewhere else entirely.

fn main() {
    let trip = std::env::args().nth(1).unwrap_or_default();
    if trip.is_empty() {
        eprintln!("usage: cargo run -p reel-core --example timeline -- <trip>");
        std::process::exit(2);
    }
    let cfg = reel_core::Config::from_env();
    match reel_core::build_timeline(&cfg, &trip) {
        Ok(t) => {
            println!("wrote    {}", t.path);
            println!(
                "profile  {}{}",
                t.profile,
                match &t.profile_id {
                    Some(id) => format!(" ({id})"),
                    None => " (no stock preset - Kdenlive opens at its default)".into(),
                }
            );
            println!(
                "segments {} from {} source(s), {} skipped",
                t.segments, t.sources, t.skipped
            );
            println!("duration {:.2}s", t.duration);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
