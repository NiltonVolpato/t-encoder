// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

slint::include_modules!();

/// Path to the theme.slint file for consumers using `slint-build`.
pub const THEME_SLINT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui/theme.slint");
