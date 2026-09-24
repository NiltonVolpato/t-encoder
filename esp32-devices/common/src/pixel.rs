// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Display pixel types for Slint rendering and DMA transfers.

use slint::platform::software_renderer::{PremultipliedRgbaColor, Rgb565Pixel, TargetPixel};

use crate::simd;

/// RGB565 stored in the display controller's byte order (big-endian).
///
/// Slint's `software_renderer` is generic over [`TargetPixel`], so Slint can
/// render directly into the panel's pixel format in PSRAM without requiring a
/// separate conversion pass or intermediate scratch buffer.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, defmt::Format)]
pub struct BigEndianRgb565(pub u16);

impl BigEndianRgb565 {
    /// Reads the pixel back as a native-endian [`Rgb565Pixel`].
    pub fn to_native(self) -> Rgb565Pixel {
        Rgb565Pixel(u16::from_be(self.0))
    }

    /// Stores a native-endian [`Rgb565Pixel`] in panel order.
    pub fn from_native(pixel: Rgb565Pixel) -> BigEndianRgb565 {
        BigEndianRgb565(pixel.0.to_be())
    }
}

impl TargetPixel for BigEndianRgb565 {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let mut native = self.to_native();
        native.blend(color);
        *self = BigEndianRgb565::from_native(native);
    }

    fn fill_slice(slice: &mut [Self], pixel: Self) {
        simd::fill_slice(slice, pixel);
    }

    fn blend_slice(slice: &mut [Self], color: PremultipliedRgbaColor) {
        if color.alpha <= 8 {
            return;
        }
        if color.alpha >= 247 {
            Self::fill_slice(slice, Self::from_rgb(color.red, color.green, color.blue));
            return;
        }

        let a = color.alpha as u16;
        let r = ((color.red as u16 * 255) / a).min(255) as u8;
        let g = ((color.green as u16 * 255) / a).min(255) as u8;
        let b = ((color.blue as u16 * 255) / a).min(255) as u8;
        let fg_pixel = Rgb565Pixel::from_rgb(r, g, b);
        let alpha_5bit = ((a + 4) >> 3) as u8;

        simd::blend_slice_be(slice, fg_pixel.0, alpha_5bit, |p| {
            p.blend(color);
        });
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> BigEndianRgb565 {
        BigEndianRgb565::from_native(Rgb565Pixel::from_rgb(red, green, blue))
    }
}

/// Native-endian RGB565 pixel for internal SRAM rendering with hardware SIMD fill acceleration.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeRgb565(pub Rgb565Pixel);

impl defmt::Format for NativeRgb565 {
    fn format(&self, fmt: defmt::Formatter) {
        defmt::write!(fmt, "NativeRgb565({=u16:#04x})", self.0.0);
    }
}

impl NativeRgb565 {
    pub const fn new(raw: u16) -> Self {
        Self(Rgb565Pixel(raw))
    }

    pub fn raw(self) -> u16 {
        self.0.0
    }
}

impl TargetPixel for NativeRgb565 {
    #[inline(always)]
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        self.0.blend(color);
    }

    #[inline(always)]
    fn fill_slice(slice: &mut [Self], pixel: Self) {
        simd::fill_slice(slice, pixel);
    }

    fn blend_slice(slice: &mut [Self], color: PremultipliedRgbaColor) {
        if color.alpha <= 8 {
            return;
        }
        if color.alpha >= 247 {
            Self::fill_slice(slice, Self::from_rgb(color.red, color.green, color.blue));
            return;
        }

        let a = color.alpha as u16;
        let r = ((color.red as u16 * 255) / a).min(255) as u8;
        let g = ((color.green as u16 * 255) / a).min(255) as u8;
        let b = ((color.blue as u16 * 255) / a).min(255) as u8;
        let fg_pixel = Rgb565Pixel::from_rgb(r, g, b);
        let alpha_5bit = ((a + 4) >> 3) as u8;

        simd::blend_slice_le(slice, fg_pixel.0, alpha_5bit, |p| {
            p.blend(color);
        });
    }

    #[inline(always)]
    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Self(Rgb565Pixel::from_rgb(red, green, blue))
    }
}
