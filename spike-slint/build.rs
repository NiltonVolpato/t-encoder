//! Compiles the spike's `.slint` UI. Resources are embedded for the software
//! renderer, since there is no filesystem on the device.

fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    if let Err(error) = slint_build::compile_with_config("ui/launcher.slint", config) {
        // A build script has no caller to propagate to; failing loudly is the
        // correct behaviour here.
        eprintln!("slint-build failed: {error}");
        std::process::exit(1);
    }
}
