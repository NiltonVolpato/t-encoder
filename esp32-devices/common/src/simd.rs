// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! ESP32-S3 Xtensa LX7 128-bit PIE SIMD hardware acceleration.
//!
//! Wraps routines from `third_party/esp_simd` (by Mike Liu) and Unexpected Maker / Larry Bank
//! to accelerate pixel buffer operations using 128-bit vector registers (`q0`–`q7`).

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

    /// Blends a constant RGB565 color over big-endian destination pixels using PIE SIMD.
    /// Requires `bg` and `dest` to be 16-byte aligned, count a multiple of 8.
    pub fn s3_alpha_blend_color_be(
        fg_color: u16,
        bg: *const u16,
        dest: *mut u16,
        count: u32,
        alpha: u8,
        masks: *const u16,
    );

    /// Blends a constant RGB565 color over native/little-endian destination pixels using PIE SIMD.
    /// Requires `bg` and `dest` to be 16-byte aligned, count a multiple of 8.
    pub fn s3_alpha_blend_color_le(
        fg_color: u16,
        bg: *const u16,
        dest: *mut u16,
        count: u32,
        alpha: u8,
        masks: *const u16,
    );

    /// Copies an array of 16-bit pixels, swapping bytes within each 16-bit word using PIE SIMD.
    /// Requires `src` and `dst` to be 16-byte aligned, count a multiple of 8.
    pub fn s3_simd_bswap16(src: *const u16, dst: *mut u16, count: u32);
}

/// Bitmasks used by SIMD alpha blending: [Blue, Green, Shifted Red, Red].
pub static COLOR_MASKS: [u16; 4] = [0x001F, 0x07E0, 0x07C0, 0xF800];

/// Enables Coprocessor 3 (PIE - Processor Instruction Extension) on the ESP32-S3 LX7 core.
pub fn enable_pie() {
    #[cfg(target_arch = "xtensa")]
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
        #[cfg(target_arch = "xtensa")]
        unsafe {
            simd_fill_i16(ptr as *mut i16, &val_raw, remaining);
        }
        #[cfg(not(target_arch = "xtensa"))]
        {
            for i in 0..remaining {
                unsafe {
                    *ptr.add(i) = value;
                }
            }
        }
    }
}

/// Blends a slice of big-endian RGB565 pixels with a constant color using 128-bit PIE SIMD.
#[inline]
pub fn blend_slice_be<T: Copy, F: Fn(&mut T)>(
    slice: &mut [T],
    fg_color: u16,
    alpha_5bit: u8,
    scalar_fallback: F,
) {
    assert_eq!(core::mem::size_of::<T>(), 2);
    let len = slice.len();
    if len == 0 {
        return;
    }

    let mut ptr = slice.as_mut_ptr();
    let mut remaining = len;

    // 1. Scalar blend unaligned prefix to reach 16-byte (128-bit) alignment
    while (ptr as usize & 0xF) != 0 && remaining > 0 {
        unsafe {
            scalar_fallback(&mut *ptr);
            ptr = ptr.add(1);
        }
        remaining -= 1;
    }

    // 2. SIMD blend 8-pixel blocks
    let simd_count = remaining & !7;
    if simd_count > 0 {
        #[cfg(target_arch = "xtensa")]
        unsafe {
            let u16_ptr = ptr as *mut u16;
            s3_alpha_blend_color_be(
                fg_color,
                u16_ptr,
                u16_ptr,
                simd_count as u32,
                alpha_5bit,
                COLOR_MASKS.as_ptr(),
            );
        }
        #[cfg(not(target_arch = "xtensa"))]
        {
            let _ = (fg_color, alpha_5bit);
            for i in 0..simd_count {
                unsafe {
                    scalar_fallback(&mut *ptr.add(i));
                }
            }
        }
        unsafe {
            ptr = ptr.add(simd_count);
        }
        remaining -= simd_count;
    }

    // 3. Scalar blend any trailing elements
    while remaining > 0 {
        unsafe {
            scalar_fallback(&mut *ptr);
            ptr = ptr.add(1);
        }
        remaining -= 1;
    }
}

/// Blends a slice of native/little-endian RGB565 pixels with a constant color using 128-bit PIE SIMD.
#[inline]
pub fn blend_slice_le<T: Copy, F: Fn(&mut T)>(
    slice: &mut [T],
    fg_color: u16,
    alpha_5bit: u8,
    scalar_fallback: F,
) {
    assert_eq!(core::mem::size_of::<T>(), 2);
    let len = slice.len();
    if len == 0 {
        return;
    }

    let mut ptr = slice.as_mut_ptr();
    let mut remaining = len;

    // 1. Scalar blend unaligned prefix to reach 16-byte (128-bit) alignment
    while (ptr as usize & 0xF) != 0 && remaining > 0 {
        unsafe {
            scalar_fallback(&mut *ptr);
            ptr = ptr.add(1);
        }
        remaining -= 1;
    }

    // 2. SIMD blend 8-pixel blocks
    let simd_count = remaining & !7;
    if simd_count > 0 {
        #[cfg(target_arch = "xtensa")]
        unsafe {
            let u16_ptr = ptr as *mut u16;
            s3_alpha_blend_color_le(
                fg_color,
                u16_ptr,
                u16_ptr,
                simd_count as u32,
                alpha_5bit,
                COLOR_MASKS.as_ptr(),
            );
        }
        #[cfg(not(target_arch = "xtensa"))]
        {
            let _ = (fg_color, alpha_5bit);
            for i in 0..simd_count {
                unsafe {
                    scalar_fallback(&mut *ptr.add(i));
                }
            }
        }
        unsafe {
            ptr = ptr.add(simd_count);
        }
        remaining -= simd_count;
    }

    // 3. Scalar blend any trailing elements
    while remaining > 0 {
        unsafe {
            scalar_fallback(&mut *ptr);
            ptr = ptr.add(1);
        }
        remaining -= 1;
    }
}

/// Copies a slice of 16-bit pixels, swapping byte order from native to big-endian (or vice versa).
/// Uses 128-bit PIE SIMD when pointers share 16-byte alignment, 32-bit dual-pixel bitwise swap
/// when 4-byte aligned, and scalar fallback for remaining odd elements.
pub fn bswap16_copy_slice(src: &[u16], dst: &mut [u16]) {
    assert_eq!(src.len(), dst.len());
    let mut len = src.len();
    if len == 0 {
        return;
    }

    let mut src_ptr = src.as_ptr();
    let mut dst_ptr = dst.as_mut_ptr();

    // Check if src and dst share the same 16-byte alignment offset
    let src_misalign = (src_ptr as usize) & 0xF;
    let dst_misalign = (dst_ptr as usize) & 0xF;

    if src_misalign == dst_misalign {
        // Scalar prefix to reach 16-byte alignment
        while ((src_ptr as usize) & 0xF) != 0 && len > 0 {
            unsafe {
                *dst_ptr = (*src_ptr).to_be();
                src_ptr = src_ptr.add(1);
                dst_ptr = dst_ptr.add(1);
            }
            len -= 1;
        }

        // SIMD 8-pixel blocks
        let simd_count = len & !7;
        if simd_count > 0 {
            #[cfg(target_arch = "xtensa")]
            unsafe {
                s3_simd_bswap16(src_ptr, dst_ptr, simd_count as u32);
            }
            #[cfg(not(target_arch = "xtensa"))]
            {
                for i in 0..simd_count {
                    unsafe {
                        *dst_ptr.add(i) = (*src_ptr.add(i)).to_be();
                    }
                }
            }
            unsafe {
                src_ptr = src_ptr.add(simd_count);
                dst_ptr = dst_ptr.add(simd_count);
            }
            len -= simd_count;
        }
    }

    // Align both to 4 bytes if both have odd 16-bit word offset
    if ((src_ptr as usize) & 2) != 0 && ((dst_ptr as usize) & 2) != 0 && len > 0 {
        unsafe {
            *dst_ptr = (*src_ptr).to_be();
            src_ptr = src_ptr.add(1);
            dst_ptr = dst_ptr.add(1);
        }
        len -= 1;
    }

    // Process 32-bit words (2 pixels at a time) if both are 4-byte aligned
    if ((src_ptr as usize) & 0x3) == 0 && ((dst_ptr as usize) & 0x3) == 0 {
        let words = len / 2;
        let mut src_w = src_ptr as *const u32;
        let mut dst_w = dst_ptr as *mut u32;
        for _ in 0..words {
            unsafe {
                let w = *src_w;
                // Swap bytes within each 16-bit halfword:
                // [b0, b1, b2, b3] -> [b1, b0, b3, b2]
                *dst_w = ((w & 0xFF00_FF00) >> 8) | ((w & 0x00FF_00FF) << 8);
                src_w = src_w.add(1);
                dst_w = dst_w.add(1);
            }
        }
        src_ptr = src_w as *const u16;
        dst_ptr = dst_w as *mut u16;
        len %= 2;
    }

    // Scalar tail for any remaining odd pixel
    while len > 0 {
        unsafe {
            *dst_ptr = (*src_ptr).to_be();
            src_ptr = src_ptr.add(1);
            dst_ptr = dst_ptr.add(1);
        }
        len -= 1;
    }
}
