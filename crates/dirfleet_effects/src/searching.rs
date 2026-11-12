//! Searches through subfolders, each walk on its own thread, and the delay
//! the core waits for before recording typed text.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{Clock, Delay, ListDirOutput, SearchWalk, WalkUpdate};
use dirfleet_core::listing::Listing;

use crate::content::ContentScan;
use crate::list::display_order_with;
use crate::open_history::OpenHistory;
use crate::walk::{Found, WalkOptions, walk};

/// The threads that look inside files, apart from the ones that read and
/// sort folders, so a long content search holds no listing up.
pub fn scan_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism()
            .map_or(2, |count| count.get().saturating_sub(1).clamp(2, 8));
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("dirfleet_scan_{index}"))
            .build()
            .map_err(|error| log::warn!("no threads for searching file contents: {error}"))
            .ok()
    })
    .as_ref()
}

type Start = dyn Fn(Request<SearchWalk>, Arc<AtomicBool>) + Send + Sync;

/// Runs walks until they finish or are stopped.
pub struct WalkHandler {
    running: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    start: Box<Start>,
}

impl WalkHandler {
    pub fn new<R>(sink: Weak<R>, history: Arc<OpenHistory>) -> Self
    where
        R: ResolveSink<SearchWalk> + Send + Sync + 'static,
    {
        let running: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>> = Arc::default();
        let finished = Arc::downgrade(&running);
        Self {
            running,
            start: Box::new(move |request, cancel| {
                spawn_walk(
                    request,
                    cancel,
                    sink.clone(),
                    finished.clone(),
                    Arc::clone(&history),
                );
            }),
        }
    }

    pub fn start(&self, request: Request<SearchWalk>) {
        let cancel = Arc::new(AtomicBool::new(false));
        lock(&self.running).insert(request.operation.id, Arc::clone(&cancel));
        (self.start)(request, cancel);
    }

    pub fn stop(&self, id: u64) {
        let cancel = lock(&self.running).remove(&id);
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// How many walks are running.
    pub fn count(&self) -> usize {
        lock(&self.running).len()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn_walk<R>(
    mut request: Request<SearchWalk>,
    cancel: Arc<AtomicBool>,
    sink: Weak<R>,
    running: Weak<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    history: Arc<OpenHistory>,
) where
    R: ResolveSink<SearchWalk> + Send + Sync + 'static,
{
    let id = request.operation.id;
    let spawned = std::thread::Builder::new()
        .name(format!("dirfleet_walk_{id}"))
        .spawn(move || {
            let operation = request.operation.clone();
            let roots: Vec<PathBuf> = operation.roots.iter().map(PathBuf::from).collect();
            let depth = operation.depth as usize;
            let root = roots
                .first()
                .map(|root| crate::paths::normalize_path_string(root))
                .unwrap_or_default();
            let update = |found: Found, done: bool| {
                let listing: Arc<Listing> = found.listing;
                WalkUpdate {
                    output: ListDirOutput::Listed {
                        order: display_order_with(&listing, &operation.view, history.as_ref()),
                        opened: Arc::new(history.keys_if_any(&listing)),
                        listing,
                        view: operation.view.clone(),
                    },
                    done,
                    root: root.clone(),
                    matched: found.matched,
                    scanned: found.scanned,
                }
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs());
            let scan = operation
                .content
                .as_deref()
                .and_then(|search| ContentScan::new(search, now));
            let options = WalkOptions {
                content: scan.as_ref(),
                skip_ignored: operation.skip_ignored,
                ..WalkOptions::default()
            };
            // Partial results go out from the walk's threads, one at a time.
            let request = Mutex::new(&mut request);
            let resolve = |update: WalkUpdate| {
                sink.upgrade()
                    .is_some_and(|sink| sink.resolve_request(&mut **lock(&request), update).is_ok())
            };
            let on_partial = |partial: Found| {
                if !cancel.load(Ordering::Relaxed) && !resolve(update(partial, false)) {
                    cancel.store(true, Ordering::Relaxed);
                }
            };
            let run = || {
                walk(
                    &roots,
                    depth,
                    operation.view.show_hidden,
                    &cancel,
                    options,
                    &on_partial,
                )
            };
            // Looking inside files takes its own threads.
            let found = scan_pool()
                .filter(|_| scan.is_some())
                .map_or_else(run, |pool| pool.install(run));
            if !cancel.load(Ordering::Relaxed) {
                let found = Found {
                    listing: found.listing,
                    matched: found.matched,
                    scanned: found.scanned,
                };
                resolve(update(found, true));
            }
            if let Some(running) = running.upgrade() {
                lock(&running).remove(&id);
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not start a search: {error}");
    }
}

/// How often the clock ticks: often enough for "Just now" and "5 min ago".
const TICK: Duration = Duration::from_secs(30);

/// Tells the core the time now, then every [`TICK`], for as long as it runs.
pub fn clock<R>(mut request: Request<Clock>, sink: Weak<R>)
where
    R: ResolveSink<Clock> + Send + Sync + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("dirfleet_clock".into())
        .spawn(move || {
            loop {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| since.as_secs());
                let Some(sink) = sink.upgrade() else {
                    return;
                };
                if sink.resolve_request(&mut request, now).is_err() {
                    return;
                }
                drop(sink);
                std::thread::sleep(TICK);
            }
        });
    if let Err(error) = spawned {
        log::warn!("could not start the clock: {error}");
    }
}

/// Answers every delay once it has passed, from one thread: a hover, a
/// typed letter and a toast each ask for one, and a thread apiece was a
/// thread started and ended several times a second.
pub struct Timer {
    due: std::sync::mpsc::Sender<Due>,
}

/// A delay and when it has passed; the earliest is the greatest, for the
/// heap, and equal times keep their order.
struct Due {
    at: std::time::Instant,
    order: u64,
    answer: Box<dyn FnOnce() + Send>,
}

impl PartialEq for Due {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.order) == (other.at, other.order)
    }
}

impl Eq for Due {}

impl PartialOrd for Due {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Due {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (other.at, other.order).cmp(&(self.at, self.order))
    }
}

impl Timer {
    /// Starts the timer's thread, which ends when the timer is dropped.
    pub fn new() -> Self {
        use std::sync::mpsc::RecvTimeoutError;
        let (due, asked) = std::sync::mpsc::channel::<Due>();
        let spawned = std::thread::Builder::new()
            .name("dirfleet_timer".into())
            .spawn(move || {
                let mut waiting = std::collections::BinaryHeap::<Due>::new();
                let mut order = 0;
                loop {
                    let next = waiting.peek().map_or_else(
                        || asked.recv().map_err(|_| RecvTimeoutError::Disconnected),
                        |first| {
                            let now = std::time::Instant::now();
                            asked.recv_timeout(first.at.saturating_duration_since(now))
                        },
                    );
                    match next {
                        Ok(mut due) => {
                            order += 1;
                            due.order = order;
                            waiting.push(due);
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                    while waiting
                        .peek()
                        .is_some_and(|first| first.at <= std::time::Instant::now())
                    {
                        if let Some(due) = waiting.pop() {
                            (due.answer)();
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            log::warn!("could not start the timer: {error}");
        }
        Self { due }
    }

    /// Answers `request` once its time has passed.
    pub fn delay<R>(&self, mut request: Request<Delay>, sink: Weak<R>)
    where
        R: ResolveSink<Delay> + Send + Sync + 'static,
    {
        let at = std::time::Instant::now() + Duration::from_millis(request.operation.millis);
        let answer = Box::new(move || {
            if let Some(sink) = sink.upgrade() {
                let _ = sink.resolve_request(&mut request, ());
            }
        });
        let _ = self.due.send(Due {
            at,
            order: 0,
            answer,
        });
    }
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}
