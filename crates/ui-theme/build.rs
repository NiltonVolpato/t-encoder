// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

fn main() {
    let mut config = slint_build::CompilerConfiguration::new()
        .as_library("theme");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" {
        config = config.embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    }

    slint_build::compile_with_config("ui/theme.slint", config).unwrap();
}
