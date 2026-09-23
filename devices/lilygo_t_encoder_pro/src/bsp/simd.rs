// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! ESP32-S3 Xtensa LX7 128-bit PIE SIMD hardware acceleration.
//!
//! Wraps routines from `third_party/esp_simd` (by Mike Liu) to accelerate
//! pixel buffer operations using 128-bit vector registers (`q0`–`q7`).

unsafe extern "C" {
    /// Fills an `int16_t` array with a constant value using PIE SIMD.
    /// Requires `a` to be 16-byte (128-bit) aligned.
    pub fn simd_fill_i16(a: *mut i16, val: *const i16, size: usize) -> i32;

    /// Zeros an `int16_t` array using PIE SIMD.
    /// Requires `a` to be 16-byte (128-bit) aligned.
    pub fn simd_zeros_i16(a: *mut i16, size: usize) -> i32;

    /// Copies an `int16_t` array using PIE SIMD.
    /// Requires `a` and `result` to be 16-byte (128-bit) aligned.
    pub fn simd_copy_i16(a: *const i16, result: *mut i16, size: usize) -> i32;
}

/// Enables Coprocessor 3 (PIE - Processor Instruction Extension) on the ESP32-S3 LX7 core.
pub fn enable_pie() {
    unsafe {
        core::arch::asm!(
            "rsr.cpenable {tmp}",
            "movi.n {mask}, 8", // bit 3 enables PIE coprocessor
            "or {tmp}, {tmp}, {mask}",
            "wsr.cpenable {tmp}",
            tmp = out(reg) _,
            mask = out(reg) _,
        );
    }
}

/// Fills a slice of 16-bit pixels using 128-bit PIE SIMD instructions.
///
/// Automatically handles 0..7 unaligned prefix pixels with scalar stores to reach
/// a 16-byte boundary, then invokes `simd_fill_i16` for hardware-accelerated 128-bit
/// stores (8 pixels per loop iteration).
#[inline]
pub fn fill_slice<T: Copy>(slice: &mut [T], value: T) {
    assert_eq!(core::mem::size_of::<T>(), 2);
    let len = slice.len();
    if len == 0 {
        return;
    }

    let mut ptr = slice.as_mut_ptr();
    let mut remaining = len;

    // 1. Scalar fill unaligned prefix to reach 16-byte (128-bit) alignment
    while (ptr as usize & 0xF) != 0 && remaining > 0 {
        unsafe {
            *ptr = value;
            ptr = ptr.add(1);
        }
        remaining -= 1;
    }

    // 2. SIMD fill the remaining 16-byte aligned slice
    if remaining > 0 {
        let val_raw = unsafe { core::mem::transmute_copy::<T, i16>(&value) };
        unsafe {
            simd_fill_i16(ptr as *mut i16, &val_raw, remaining);
        }
    }
}
