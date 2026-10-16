//! Global allocator for profile-loading binaries: the platform's own (Rust's
//! [`System`] on a desktop, and on wasm32 the Rust dlmalloc std builds in),
//! with no C allocator to compile, so every target builds the same crates.
//! Each binary declares it itself:
//!
//! ```ignore
//! #[global_allocator]
//! static GLOBAL: bviz_alloc::Allocator = bviz_alloc::Allocator;
//! ```
//!
//! glibc malloc returns large freed buffers to the OS eagerly, and that
//! munmap/madvise churn is a noticeable share of a big profile load.
//! [`retain_freed_memory_across_load`] tunes glibc to keep them instead.

use std::alloc::{GlobalAlloc, Layout, System};

/// The platform allocator, under the name the binaries declare.
pub struct Allocator;

// SAFETY: every call forwards to `System`, whose contract is this trait's.
unsafe impl GlobalAlloc for Allocator {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` is a live `System` block of `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller upholds `realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Has the allocator hold freed memory, so that a load burst reuses its
/// transient buffers (worker pools, decode buffers) instead of paying the
/// release-and-refault round-trip mid-load. On glibc that is raising the trim
/// threshold, past which free hands the heap's top back to the OS, and the
/// mmap threshold, past which a block is mapped (and unmapped on free) on its
/// own, to their ceilings. Elsewhere the allocator already keeps freed memory
/// for reuse, and this does nothing.
///
/// # Safety
///
/// No other thread may be running: glibc writes these parameters
/// unsynchronized, and every thread allocating reads them.
pub unsafe fn retain_freed_memory_across_load() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        /// Free space the heap's top keeps before it is trimmed.
        const TRIM_THRESHOLD: libc::c_int = 1 << 30;
        /// glibc's ceiling for the mmap threshold on 64-bit targets
        /// (`DEFAULT_MMAP_THRESHOLD_MAX`); a larger value is refused.
        const MMAP_THRESHOLD: libc::c_int = 32 << 20;
        // SAFETY: mallopt takes any parameter and value, and the caller
        // guarantees no other thread reads the parameters it writes.
        unsafe {
            libc::mallopt(libc::M_TRIM_THRESHOLD, TRIM_THRESHOLD);
            libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_reallocates_and_frees() {
        // SAFETY: no other thread runs in this test binary's main thread
        // before the allocations below; glibc's parameters only steer reuse.
        unsafe { retain_freed_memory_across_load() };
        let mut v: Vec<u64> = Vec::with_capacity(1 << 20);
        v.extend(0..1 << 20);
        v.reserve(1 << 22);
        assert_eq!(v.iter().sum::<u64>(), (1u64 << 20) * ((1 << 20) - 1) / 2);
        let boxed = vec![0u8; 64 << 20].into_boxed_slice();
        assert!(boxed.iter().all(|&b| b == 0));
    }
}
