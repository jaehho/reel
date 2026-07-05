//! Quick check: print what the engine sees, as the UI will receive it.
//! `cargo run -p reel-core --example dump`

fn main() {
    let cfg = reel_core::Config::from_env();
    let trips = reel_core::list_trips(&cfg);
    println!(
        "=== trips ===\n{}",
        serde_json::to_string_pretty(&trips).unwrap()
    );
    let card = reel_core::scan_card(&cfg);
    println!(
        "=== card ===\n{}",
        serde_json::to_string_pretty(&card).unwrap()
    );
}
