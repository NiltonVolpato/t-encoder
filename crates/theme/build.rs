fn main() -> Result<(), Box<dyn std::error::Error>> {
    baker::bake_library("theme", "ui/theme.slint")
}
