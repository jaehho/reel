// No extra console window on Windows in release; harmless elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clip_server;
mod commands;

/// Stop the webview from zooming the whole UI on a touchpad pinch, and hand the
/// gesture to the front end so it can scale the clip on the stage instead.
///
/// On Linux a pinch never reaches the DOM: GDK delivers it as a `GDK_TOUCHPAD_PINCH`
/// event and WebKitGTK claims it with a `GtkGestureZoom` of its own (it imports
/// `gtk_gesture_zoom_new` — check with `nm -D -u` on libwebkit2gtk). So there's no
/// `wheel` event to `preventDefault`, and watching the WebView's `zoom-level`
/// property doesn't help either: WebKit scales the page internally without ever
/// going through the GObject setter, so `notify::zoom-level` stays silent.
///
/// So the gesture has to be intercepted at the GTK layer, and it takes two pieces:
///   1. A capture-phase `GestureZoom` of our own turns the raw pinch into a scale
///      factor we can forward to the UI. Capture runs before the target widget's
///      own controllers, so we see it first.
///   2. That is *not* enough to stop WebKit — a claimed capture-phase gesture was
///      observed still letting the event through to the `event` signal. So the
///      `event` handler returns `Stop`, which halts the RUN_LAST emission before
///      the class closure where WebKit does its zooming.
///
/// `zoom-level` is still pinned as a backstop, for ctrl+= and anything else that
/// drives the property directly.
///
/// Set `REEL_ZOOM_DEBUG=1` to log what GDK actually delivers here — the one thing
/// that can't be checked without a touchpad.
#[cfg(target_os = "linux")]
fn pin_page_zoom(app: &tauri::App) {
    use gtk::prelude::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use tauri::{Emitter, Manager};
    use webkit2gtk::WebViewExt;

    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let handle = window.clone();
    let _ = window.with_webview(move |wv| {
        let view = wv.inner();
        let debug = std::env::var_os("REEL_ZOOM_DEBUG").is_some();

        // GtkGesture only sees touchpad gestures if the window asks for them, and
        // we can't assume WebKit's mask is ours to inherit.
        view.add_events(gtk::gdk::EventMask::TOUCHPAD_GESTURE_MASK);

        // Claiming the gesture in the capture phase turned out NOT to stop
        // propagation — the debug build showed every pinch still arriving here, at
        // the `event` signal, which runs after capture. So this is where it dies.
        // A handler connected with g_signal_connect runs before the class closure
        // on a RUN_LAST signal, and returning Stop halts emission, so WebKit's own
        // handling never runs. The capture gesture above still sees the event first
        // and does the useful work (turning it into a scale factor).
        view.connect_event(move |_, ev| {
            let kind = ev.event_type();
            if debug && matches!(kind, gtk::gdk::EventType::TouchpadPinch) {
                eprintln!("reel: swallowing gdk {kind:?}");
            }
            if kind == gtk::gdk::EventType::TouchpadPinch {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });

        // `scale-changed` reports scale relative to the start of the gesture, but
        // the UI multiplies its own scale by what we send — so send the step since
        // the last callback, and reset the reference on each new gesture.
        let last = Rc::new(Cell::new(1.0_f64));
        let zoom = gtk::GestureZoom::builder()
            .widget(&view)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        {
            let last = last.clone();
            zoom.connect_begin(move |_, _| last.set(1.0));
        }
        {
            let last = last.clone();
            let handle = handle.clone();
            zoom.connect_scale_changed(move |_, scale| {
                let prev = last.get();
                if !(scale.is_finite() && scale > 0.0 && prev > 0.0) {
                    return;
                }
                last.set(scale);
                if debug {
                    eprintln!("reel: pinch scale {scale:.3} step {:.3}", scale / prev);
                }
                let _ = handle.emit("pinch-zoom", scale / prev);
            });
        }
        // GTK3 widgets hold only a weak pointer to their controllers, so dropping
        // this handle would quietly unhook the gesture. It lives as long as the app.
        std::mem::forget(zoom);

        // Backstop: anything that moves the property itself gets snapped back.
        // Setting it re-enters this handler once with 1.0, which returns — no loop.
        view.connect_zoom_level_notify(move |view| {
            let z = view.zoom_level();
            if (z - 1.0).abs() < 1e-6 {
                return;
            }
            if debug {
                eprintln!("reel: zoom-level moved to {z:.3}, pinning back to 1");
            }
            view.set_zoom_level(1.0);
            let _ = handle.emit("pinch-zoom", z);
        });
    });
}

fn main() {
    // Before anything that might fail: stderr dies with the terminal, and most of
    // reel's work happens on background threads whose panics nobody ever sees.
    let cfg = reel_core::Config::from_env();
    reel_core::log::init(&cfg);
    reel_core::log::event(
        reel_core::log::Level::Info,
        "tauri",
        "reel started",
        Some(serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "lib": cfg.lib.display().to_string(),
        })),
    );

    // Stream library clips to the <video> player over a loopback HTTP server:
    // WebKitGTK's media backend can't load a custom URI scheme (WebKit bug
    // 146351), but it plays http://127.0.0.1 with range/seek. serve_clip
    // scope-guards every path to the library.
    let clip_base = clip_server::start().expect("failed to start clip server");
    eprintln!("reel: serving clips on {clip_base}");
    reel_core::log::info("tauri", &format!("clip server on {clip_base}"));

    tauri::Builder::default()
        .setup(|_app| {
            #[cfg(target_os = "linux")]
            pin_page_zoom(_app);
            Ok(())
        })
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
            commands::card_playlist,
            commands::make_proxy,
            commands::make_card_proxy,
            commands::cut_trip,
            commands::grab_still,
            commands::open_in_editor,
            commands::clip_health,
            commands::move_clips,
            commands::rename_trip,
            commands::merge_trips,
            commands::delete_clips,
            commands::delete_trip,
            commands::clear_discarded,
            commands::cloud_contributors,
            commands::pull_person,
            commands::sync_status,
            commands::sync_trip,
            commands::sync_all,
            commands::dedup_scan,
            commands::dedup_resolve,
            commands::sharing_status,
            commands::trip_shares,
            commands::share_add,
            commands::share_remove,
            commands::share_friends,
            commands::sharee_search,
            commands::log_event,
            commands::log_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running reel");
}
