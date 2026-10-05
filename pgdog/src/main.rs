//! pgDog, modern PostgreSQL proxy, pooler and query router.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_jemalloc_background_thread();
    pgdog::cli()
}

#[cfg(not(test))]
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Enable jemalloc's background purge threads so freed memory is returned to
/// the OS after allocation bursts.
#[cfg(all(not(test), not(target_env = "msvc")))]
fn enable_jemalloc_background_thread() {
    let _ = tikv_jemalloc_ctl::background_thread::write(true);
}

/// No-op fallback where jemalloc is not the active allocator.
#[cfg(any(test, target_env = "msvc"))]
fn enable_jemalloc_background_thread() {}
