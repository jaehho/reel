//! A stop token for the long jobs — sync, share, cut (#56).
//!
//! Checked at file boundaries and between trips, never mid-transfer: a stop
//! lets the file in flight finish (a running rclone gets SIGINT, which is
//! exactly "after the current file") and then unwinds with whatever landed.
//! One token for every job, so the Stop in the sync panel is the same switch
//! the share queue and cut respect.

use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

/// Arm the stop. Jobs notice at their next file boundary.
pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
}

/// True while a stop is armed.
pub fn stopped() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Disarm. The entry point of each *user action* calls this — a sweep, a cut,
/// a share drain — so one run's Stop can never cancel the next run. Jobs that
/// run inside another job (a per-trip reconcile inside the sweep) must not.
pub fn reset_stop() {
    STOP.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_arms_and_clears() {
        reset_stop();
        assert!(!stopped());
        request_stop();
        assert!(stopped());
        reset_stop();
        assert!(!stopped());
    }
}
