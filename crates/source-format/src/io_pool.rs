//! Bounded Rayon thread pool dedicated to filesystem I/O work.
//!
//! Keeps directory walks, reads, and writes within an OS-friendly concurrency cap (4 threads on
//! macOS by default) to avoid APFS vnode lock contention while CPU-heavy stages (XML work, sorting,
//! DEFLATE compression) continue to use the full global Rayon pool.

use std::num::NonZeroUsize;
use std::sync::OnceLock;

static IO_POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();

fn target_io_threads() -> usize {
    if let Ok(raw) = std::env::var("RAYON_NUM_THREADS")
        && let Ok(parsed) = raw.parse::<usize>()
        && parsed > 0
    {
        return parsed;
    }
    let available = std::thread::available_parallelism().map_or(4, NonZeroUsize::get);
    if cfg!(target_os = "macos") {
        available.min(4)
    } else {
        available.min(8)
    }
}

fn io_pool() -> Option<&'static rayon::ThreadPool> {
    IO_POOL
        .get_or_init(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(target_io_threads())
                .thread_name(|idx| format!("sf-io-{idx}"))
                .build()
                .ok()
        })
        .as_ref()
}

/// Runs `op` inside the bounded I/O Rayon pool, falling back inline if pool creation fails.
pub(crate) fn io_install<OP, R>(op: OP) -> R
where
    OP: FnOnce() -> R + Send,
    R: Send,
{
    match io_pool() {
        Some(pool) if pool.current_thread_index().is_none() => pool.install(op),
        _ => op(),
    }
}
