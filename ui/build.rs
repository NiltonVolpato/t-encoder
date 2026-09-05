//! Compiles the whole `.slint` tree in one pass, from the single root
//! `shell.slint`. Every app's UI is imported from there, which is what keeps
//! the Slint runtime, font data and ICU tables linked once rather than per app.

fn main() {
    let config = slint_build::CompilerConfiguration::new()
        // No filesystem on the device: fonts and images are baked into flash.
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    if let Err(error) = slint_build::compile_with_config("ui/shell.slint", config) {
        // A build script has no caller to propagate to; failing loudly is
        // correct here.
        eprintln!("slint-build failed: {error}");
        std::process::exit(1);
    }
}
