use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    baker::bake_library("app_clock", "ui/clock.slint")?;

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")?;
    let out_dir = std::env::var("OUT_DIR")?;
    let img_path = Path::new(&manifest_dir).join("assets/sf-bg.jpg");
    println!("cargo:rerun-if-changed={}", img_path.display());

    let img = image::open(&img_path)?;

    let img_360 = img.resize_exact(360, 360, image::imageops::FilterType::Triangle);
    let out_360 = Path::new(&out_dir).join("bg_360.jpg");
    img_360.save_with_format(&out_360, image::ImageFormat::Jpeg)?;

    let img_390 = img.resize_exact(390, 390, image::imageops::FilterType::Triangle);
    let out_390 = Path::new(&out_dir).join("bg_390.jpg");
    img_390.save_with_format(&out_390, image::ImageFormat::Jpeg)?;

    Ok(())
}
