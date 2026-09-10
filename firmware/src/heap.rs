//! Internal heap configuration constants for the radio and RTOS tasks.
//!
//! PSRAM is registered as Region 0 in `esp_alloc::HEAP`, serving as the primary
//! allocator for general allocations. The internal SRAM regions defined here
//! are registered after PSRAM for radio and DMA allocations requiring internal memory.

/// Internal global-heap region reclaimed from bootloader RAM (the full
/// `dram2_seg`; larger overflows it). First of two `heap_allocator!` regions.
pub const INTERNAL_HEAP_RECLAIMED: usize = 73_744;

/// Second internal region (a static `.bss` array in main DRAM) added on top of
/// the reclaimed RAM — esp-radio Wi-Fi+BLE coex needs more than the reclaimed
/// region alone (radio task stacks alloc from the global heap via `esp-rtos`).
pub const INTERNAL_HEAP_EXTRA: usize = 64 * 1024;
