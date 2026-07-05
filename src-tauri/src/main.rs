// No extra console window on Windows in release; harmless elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clip_server;
mod commands;

fn main() {
    // Stream library clips to the <video> player over a loopback HTTP server:
    // WebKitGTK's media backend can't load a custom URI scheme (WebKit bug
    // 146351), but it plays http://127.0.0.1 with range/seek. serve_clip
    // scope-guards every path to the library.
    let clip_base = clip_server::start().expect("failed to start clip server");
    eprintln!("reel: serving clips on {clip_base}");

    tauri::Builder::default()
        .manage(commands::ClipBase(clip_base))
        .invoke_handler(tauri::generate_handler![
            commands::clip_base,
            commands::list_trips,
            commands::scan_card,
            commands::thumb,
            commands::import_session,
            commands::share_trip,
            commands::plan_reclaim,
            commands::commit_reclaim,
            commands::plan_archive,
            commands::commit_archive,
            commands::review_playlist,
            commands::save_marks,
            commands::make_proxy,
            commands::cut_trip,
            commands::clip_health,
        ])
        .run(tauri::generate_context!())
        .expect("error while running reel");
}
