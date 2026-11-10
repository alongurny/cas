//! How many threads the store and the commands built on it may use: all
//! cores unless a program says otherwise (the CLI's `-j N`), for example to
//! compare with a single-threaded tool.

use std::sync::atomic::{AtomicUsize, Ordering};

static THREADS: AtomicUsize = AtomicUsize::new(0);

/// Use at most `n` threads; 0 means one per core.
pub fn set(n: usize) {
    THREADS.store(n, Ordering::Relaxed);
}

/// The number of threads to use, at least 1.
pub fn get() -> usize {
    match THREADS.load(Ordering::Relaxed) {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
        n => n,
    }
}
