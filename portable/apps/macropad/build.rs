fn main() -> Result<(), Box<dyn std::error::Error>> {
    baker::bake_library("app_macropad", "ui/macropad.slint")
}
