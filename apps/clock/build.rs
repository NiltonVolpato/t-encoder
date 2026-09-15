fn main() -> Result<(), slint_build::CompileError> {
    let config = slint_build::CompilerConfiguration::new().as_library("app_clock");

    slint_build::compile_with_config("ui/clock.slint", config)
}
