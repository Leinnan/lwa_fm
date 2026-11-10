use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use crossbeam::channel::{Sender, TryRecvError, bounded, select, unbounded};
use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{Watch, WatchEvent};
use notify::{RecursiveMode, Watcher as _};

/// Changes closer together than this are reported once.
const COALESCE: Duration = Duration::from_millis(150);
/// Changes that keep coming (a download, an archive being unpacked) are
/// reported this often, so the folder shows them before they end.
const LONGEST_WAIT: Duration = Duration::from_secs(1);

/// One thread per watched folder. Dropping its sender stops it.
pub struct WatchHandler {
    start: Box<dyn Fn(Request<Watch>) -> Option<Sender<()>> + Send + Sync>,
    running: Mutex<HashMap<u64, Sender<()>>>,
}

impl WatchHandler {
    pub fn new<R>(sink: Weak<R>) -> Self
    where
        R: ResolveSink<Watch> + Send + Sync + 'static,
    {
        Self {
            start: Box::new(move |request| spawn_watch(request, sink.clone())),
            running: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(&self, request: Request<Watch>) {
        let id = request.operation.id;
        if let Some(stop) = (self.start)(request) {
            self.lock().insert(id, stop);
        }
    }

    pub fn stop(&self, id: u64) {
        self.lock().remove(&id);
    }

    /// How many folders are watched.
    pub fn count(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Sender<()>>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Watches the request's folder on a new thread until the returned sender is dropped.
fn spawn_watch<R>(mut request: Request<Watch>, sink: Weak<R>) -> Option<Sender<()>>
where
    R: ResolveSink<Watch> + Send + Sync + 'static,
{
    let (stop, stopped) = bounded::<()>(0);
    let resolve = move |request: &mut Request<Watch>, event| {
        sink.upgrade()
            .is_some_and(|sink| sink.resolve_request(request, event).is_ok())
    };
    std::thread::Builder::new()
        .name(format!("dirfleet_watch_{}", request.operation.id))
        .spawn(move || {
            // Created here, not on the caller's thread: starting a watcher
            // waits for the OS (FSEvents' run loop thread on macOS).
            let (on_change, changes) = unbounded::<notify::Result<notify::Event>>();
            let path = Path::new(&request.operation.path).to_path_buf();
            let on_change = move |event| {
                let _ = on_change.send(event);
            };
            let watcher = notify::recommended_watcher(on_change).and_then(|mut watcher| {
                watcher.watch(&path, RecursiveMode::NonRecursive)?;
                Ok(watcher)
            });
            let mut watcher = match watcher {
                Ok(watcher) => watcher,
                // A folder that can't be read says so through its listing.
                Err(_) if !path.is_dir() => return,
                Err(error) => {
                    resolve(&mut request, WatchEvent::Failed(error.to_string()));
                    return;
                }
            };
            if !resolve(&mut request, WatchEvent::Ready) {
                return;
            }
            let is_stopped = || matches!(stopped.try_recv(), Err(TryRecvError::Disconnected));
            loop {
                // Asleep until a change comes or the watch is stopped.
                select! {
                    recv(changes) -> change => if change.is_err() { break },
                    recv(stopped) -> _ => break,
                }
                // Let the burst settle, then report it once; one that
                // doesn't settle is reported as it goes.
                let began = Instant::now();
                while began.elapsed() < LONGEST_WAIT && changes.recv_timeout(COALESCE).is_ok() {}
                if is_stopped() || !resolve(&mut request, WatchEvent::Changed) {
                    break;
                }
            }
            let _ = watcher.unwatch(&path);
        })
        .ok()?;
    Some(stop)
}
