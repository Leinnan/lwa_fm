//! Background polling of the drive list. Listing drives costs a few cheap
//! system calls, so a short poll notices plugged and pulled drives (and
//! keeps free space current) without per-OS device notification plumbing.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

use super::Drive;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Owns the polling thread; dropping it stops the thread.
pub struct Monitor {
    lists: Receiver<Vec<Drive>>,
    wake: Sender<()>,
}

impl Monitor {
    /// Start polling. `initial` is the list the app already shows, so only
    /// real changes are reported.
    pub fn start(initial: Vec<Drive>) -> Self {
        let (list_tx, lists) = mpsc::channel();
        let (wake, wake_rx) = mpsc::channel::<()>();
        let spawned = thread::Builder::new()
            .name("drive-monitor".into())
            .spawn(move || poll(initial, &wake_rx, &list_tx));
        if let Err(err) = spawned {
            log::warn!("Drive monitor unavailable: {err}");
        }
        Self { lists, wake }
    }

    /// Check now instead of at the next tick, after an eject.
    pub fn refresh_now(&self) {
        _ = self.wake.send(());
    }

    /// The newest list since the last call, if the drives changed.
    pub fn latest(&self) -> Option<Vec<Drive>> {
        self.lists.try_iter().last()
    }
}

fn poll(mut last: Vec<Drive>, wake: &Receiver<()>, lists: &Sender<Vec<Drive>>) {
    loop {
        match wake.recv_timeout(POLL_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let drives = super::list();
        if drives == last {
            continue;
        }
        last.clone_from(&drives);
        if lists.send(drives).is_err() {
            return;
        }
        crate::watcher::request_repaint();
    }
}
