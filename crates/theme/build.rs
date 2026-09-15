fn main() -> Result<(), slint_build::CompileError> {
    let config = slint_build::CompilerConfiguration::new().as_library("theme");

    slint_build::compile_with_config("ui/theme.slint", config)
}
