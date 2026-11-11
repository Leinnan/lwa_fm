//! Timing notes on stderr, switched on with `DIRFLEET_TRACE=1`.
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

/// Marks the moment times are counted from. Call first thing in `main`.
pub fn start() {
    START.get_or_init(Instant::now);
}

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("DIRFLEET_TRACE").is_some())
}

/// Writes `what()` with the time since [`start`], when tracing is on.
pub fn note(what: impl FnOnce() -> String) {
    if enabled() {
        let since = START.get_or_init(Instant::now).elapsed();
        eprintln!("TRACE {:>9.1} ms  {}", since.as_secs_f64() * 1000.0, what());
    }
}

/// Runs `work` and notes how long it took, when tracing is on.
pub fn timed<T>(what: &str, work: impl FnOnce() -> T) -> T {
    if !enabled() {
        return work();
    }
    let began = Instant::now();
    let result = work();
    let took = began.elapsed().as_secs_f64() * 1000.0;
    note(|| format!("{what} took {took:.2} ms"));
    result
}
