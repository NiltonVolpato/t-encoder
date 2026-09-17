fn main() -> Result<(), Box<dyn std::error::Error>> {
    baker::bake_library("app_magic8", "ui/magic8.slint")
}
