fn main() -> Result<(), Box<dyn std::error::Error>> {
    baker::bake_library("app_launcher", "ui/launcher.slint")
}
