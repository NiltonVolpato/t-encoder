use std::path::Path;

pub fn bake_library(
    library: &str,
    slint_path: impl AsRef<Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    bake_internal(Some(library), slint_path)
}

pub fn bake(slint_path: impl AsRef<Path>) -> Result<(), Box<dyn std::error::Error>> {
    bake_internal(None, slint_path)
}

fn bake_internal(
    library: Option<&str>,
    slint_path: impl AsRef<Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = slint_build::CompilerConfiguration::new().with_sdf_fonts(true);

    if let Some(library_name) = library {
        config = config.as_library(library_name);
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "none" {
        config = config.embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    }

    slint_build::compile_with_config(slint_path, config)?;

    Ok(())
}
