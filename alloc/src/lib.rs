//! Global allocator for profile-loading binaries. glibc malloc returns large
//! freed buffers to the OS eagerly, and that munmap/madvise churn is a
//! noticeable share of a big profile load; mimalloc retains freed ranges and
//! reuses them. Each binary declares the allocator itself:
//!
//! ```ignore
//! #[global_allocator]
//! static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;
//! ```

use std::alloc::{GlobalAlloc, Layout};

use libmimalloc_sys::{mi_malloc, mi_realloc, mi_zalloc};

/// mimalloc, entered through its plain calls wherever a block's own alignment
/// suffices. mimalloc 3.3.2, as libmimalloc-sys 0.1.49 bundles it, serves an
/// aligned request from its size class's free list only up to 128 words, and
/// only while that list holds a block at the alignment; otherwise, unless the
/// block size is a power of two of at most 4 KiB, it allocates a block from a
/// larger class and aligns inside it. That never refills the requested class, so once
/// the class runs dry every later request of that size wastes the difference,
/// and most of a layout's allocations are such sizes.
pub struct MiMalloc;

/// The alignment every mimalloc block has: blocks are sized in whole machine
/// words from a page start aligned wider still.
const BLOCK_ALIGN: usize = std::mem::size_of::<usize>();

unsafe impl GlobalAlloc for MiMalloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= BLOCK_ALIGN {
            unsafe { mi_malloc(layout.size()).cast() }
        } else {
            unsafe { mimalloc::MiMalloc.alloc(layout) }
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= BLOCK_ALIGN {
            unsafe { mi_zalloc(layout.size()).cast() }
        } else {
            unsafe { mimalloc::MiMalloc.alloc_zeroed(layout) }
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { mimalloc::MiMalloc.dealloc(ptr, layout) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.align() <= BLOCK_ALIGN {
            unsafe { mi_realloc(ptr.cast(), new_size).cast() }
        } else {
            unsafe { mimalloc::MiMalloc.realloc(ptr, layout, new_size) }
        }
    }
}

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
