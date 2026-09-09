use espielberg::Director;
use std::env;

#[test]
fn launcher_screenshot_snapshot() {
    let port = env::var("FLASH_PORT").unwrap_or_else(|_| "/dev/cu.usbmodem101".to_string());
    let mut director = Director::action(&port).expect("connect to device");
    let shot = director.take().expect("capture screenshot");
    director.cut().expect("disconnect cleanly");

    let png_bytes = shot.to_png_bytes().expect("encode to png");
    insta::assert_binary_snapshot!(".png", png_bytes);
}
