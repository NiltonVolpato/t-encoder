// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! Heap setup for the radio (Phase 6).
//!
//! Two heaps, deliberately separate:
//! - the **global** `esp_alloc::HEAP` (registered by `esp_alloc::heap_allocator!`
//!   in `main`) is **internal-only** and serves esp-radio (Wi-Fi/BLE) + DMA;
//! - [`PSRAM_HEAP`] is a **separate, non-global** heap in PSRAM for the app's
//!   bulk allocations (`Vec::new_in(&PSRAM_HEAP)`), so radio allocations can
//!   never spill to PSRAM (PSRAM is unsafe for radio queues/atomics).

use core::sync::atomic::{AtomicBool, Ordering};

use esp_alloc::{EspHeap, HeapRegion, MemoryCapability};

/// Internal global-heap region reclaimed from bootloader RAM (the full
/// `dram2_seg`; larger overflows it). First of two `heap_allocator!` regions.
pub const INTERNAL_HEAP_RECLAIMED: usize = 73_744;

/// Second internal region (a static `.bss` array in main DRAM) added on top of
/// the reclaimed RAM — esp-radio Wi-Fi+BLE coex needs more than the reclaimed
/// region alone (radio task stacks alloc from the global heap via `esp-rtos`).
pub const INTERNAL_HEAP_EXTRA: usize = 128 * 1024;

/// Separate, non-global heap backed by PSRAM (External capability) for app bulk.
pub static PSRAM_HEAP: EspHeap = EspHeap::empty();

/// Guards [`init_psram_heap`] against a second registration (which would add an
/// overlapping region and let the allocator hand out aliased blocks).
static PSRAM_HEAP_INIT: AtomicBool = AtomicBool::new(false);

/// Registers a PSRAM region with [`PSRAM_HEAP`], placed after the framebuffer
/// and before the top smoke-test probe. Returns `true` if a region was added.
///
/// Idempotent: only the first call registers a region; later calls and any
/// invalid/too-small range (null base, `framebuffer_bytes + probe_len` past the
/// end) return `false` without touching the heap.
pub fn init_psram_heap(
    psram_start: *mut u8,
    psram_size: usize,
    framebuffer_bytes: usize,
    probe_len: usize,
) -> bool {
    if PSRAM_HEAP_INIT.swap(true, Ordering::SeqCst) || psram_start.is_null() {
        return false;
    }
    let reserved = framebuffer_bytes.saturating_add(probe_len);
    // `checked_sub` success also proves `framebuffer_bytes <= psram_size`, so
    // the `add` below stays within the mapped PSRAM range.
    let Some(len) = psram_size.checked_sub(reserved) else {
        return false;
    };
    if len == 0 {
        return false;
    }
    let start = unsafe { psram_start.add(framebuffer_bytes) };
    // SAFETY: `[start, start+len)` is mapped PSRAM past the framebuffer and
    // before the top probe (bounds checked above); the swap guard guarantees it
    // is registered at most once, so no other allocation aliases it.
    unsafe {
        PSRAM_HEAP.add_region(HeapRegion::new(
            start,
            len,
            MemoryCapability::External.into(),
        ));
    }
    true
}
