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
//!
//! PSRAM bring-up lives here too — [`smoke_test`] proves the chip is actually
//! talking before [`framebuffer`] hands out a slice of it.

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

/// PSRAM smoke-test probe length (written at the top of PSRAM, and reserved
/// from the heap so nothing else ever lands there).
pub const PROBE_LEN: usize = 4096;

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

/// Writes a [`PROBE_LEN`] pattern to the top of PSRAM, reads it back, and
/// reports whether it round-trips.
///
/// A failure almost always means the configured PSRAM mode (octal/quad) does
/// not match the module — this board is octal, and a quad config fails here.
pub fn smoke_test(start: *mut u8, size: usize) -> bool {
    if size < PROBE_LEN {
        log::error!("psram: too small for smoke test ({size} bytes)");
        return false;
    }
    let base = unsafe { start.add(size.saturating_sub(PROBE_LEN)) };
    for i in 0..PROBE_LEN {
        let byte = u8::try_from((i ^ 0xA5) & 0xFF).unwrap_or(0);
        unsafe { core::ptr::write_volatile(base.add(i), byte) };
    }
    for i in 0..PROBE_LEN {
        let want = u8::try_from((i ^ 0xA5) & 0xFF).unwrap_or(0);
        let got = unsafe { core::ptr::read_volatile(base.add(i)) };
        if got != want {
            log::error!("psram: mismatch at {i}: want {want:#04x} got {got:#04x}");
            return false;
        }
    }
    true
}

static FRAMEBUFFER_PTR: core::sync::atomic::AtomicPtr<u8> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());
static FRAMEBUFFER_LEN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Returns a shared slice to the PSRAM framebuffer if initialized.
#[must_use]
pub fn framebuffer_slice() -> Option<&'static [u8]> {
    let ptr = FRAMEBUFFER_PTR.load(Ordering::Acquire);
    let len = FRAMEBUFFER_LEN.load(Ordering::Acquire);
    if ptr.is_null() || len == 0 {
        None
    } else {
        Some(unsafe { core::slice::from_raw_parts(ptr, len) })
    }
}

/// Builds the PSRAM-backed framebuffer, or `None` if PSRAM is unavailable or
/// smaller than a full frame. Gating here keeps `from_raw_parts_mut` from ever
/// running on an invalid (e.g. `0..0`) range.
///
/// One buffer: Slint renders directly in the panel's byte order via a custom
/// `TargetPixel`, so no conversion pass and no second buffer are needed.
pub fn framebuffer(
    start: *mut u8,
    size: usize,
    ok: bool,
    bytes: usize,
) -> Option<&'static mut [u8]> {
    if !ok || start.is_null() || size < bytes {
        return None;
    }
    FRAMEBUFFER_PTR.store(start, Ordering::Release);
    FRAMEBUFFER_LEN.store(bytes, Ordering::Release);
    // SAFETY: `start`/`size` come from a successful `Psram` init; the region is
    // mapped for the whole program lifetime and is at least `bytes` long. The
    // framebuffer sits at the PSRAM base and never overlaps the smoke-test probe
    // (top `PROBE_LEN`). `u8` has alignment 1, so the pointer is always suitably
    // aligned.
    Some(unsafe { core::slice::from_raw_parts_mut(start, bytes) })
}
