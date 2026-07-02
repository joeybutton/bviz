//! Global allocator for profile-loading binaries. glibc malloc returns large
//! freed buffers to the OS eagerly, and that munmap/madvise churn is a
//! noticeable share of a big profile load; mimalloc retains freed ranges and
//! reuses them. Each binary declares the allocator itself:
//!
//! ```ignore
//! #[global_allocator]
//! static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;
//! ```

pub use mimalloc::MiMalloc;

// `mi_option_purge_delay` in mimalloc.h's `mi_option_e`; `libmimalloc-sys`
// exports `mi_option_set` only behind its `extended` feature and no constant
// for this option, so both are declared here.
const MI_OPTION_PURGE_DELAY: i32 = 15;

unsafe extern "C" {
    fn mi_option_set(option: i32, value: std::os::raw::c_long);
}

/// Configures mimalloc to hold freed memory long enough that a load burst
/// reuses its transient buffers (worker pools, decode buffers) instead of
/// paying the purge-and-refault round-trip mid-load, while still releasing
/// idle memory eventually.
pub fn retain_freed_memory_across_load() {
    // Comfortably past a large profile's load time.
    const PURGE_DELAY_MS: std::os::raw::c_long = 30_000;
    unsafe { mi_option_set(MI_OPTION_PURGE_DELAY, PURGE_DELAY_MS) }
}
