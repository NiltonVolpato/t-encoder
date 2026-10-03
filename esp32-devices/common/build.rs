fn main() {
    println!("cargo::rerun-if-changed=src/asm/simd_fill_color.S");
    println!("cargo::rerun-if-changed=src/asm/s3_simd_alphablend_color_be.S");
    println!("cargo::rerun-if-changed=src/asm/s3_simd_alphablend_color_le.S");
    println!("cargo::rerun-if-changed=src/asm/s3_simd_bswap16.S");

    let target = std::env::var("TARGET").unwrap_or_default();
    if target.starts_with("xtensa") {
        cc::Build::new()
            .compiler("xtensa-esp32s3-elf-gcc")
            .file("src/asm/simd_fill_color.S")
            .file("src/asm/s3_simd_alphablend_color_be.S")
            .file("src/asm/s3_simd_alphablend_color_le.S")
            .file("src/asm/s3_simd_bswap16.S")
            .compile("s3_simd");
    }

    println!("cargo::rerun-if-changed=build.rs");
}
