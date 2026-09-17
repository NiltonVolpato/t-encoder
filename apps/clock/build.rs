fn main() -> Result<(), slint_build::CompileError> {
    let mut config = slint_build::CompilerConfiguration::new().as_library("app_clock");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" {
        config = config.embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    }

    slint_build::compile_with_config("ui/clock.slint", config)
}
