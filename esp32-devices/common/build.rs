fn main() {
    println!(
        "cargo::rerun-if-changed=../../third_party/esp_simd/src/vector/vector_i16/simd_fill_i16.S"
    );
    println!(
        "cargo::rerun-if-changed=../../third_party/esp_simd/src/vector/vector_i16/simd_zeros_i16.S"
    );
    println!(
        "cargo::rerun-if-changed=../../third_party/esp_simd/src/vector/vector_i16/simd_copy_i16.S"
    );

    let target = std::env::var("TARGET").unwrap_or_default();
    if target.starts_with("xtensa") {
        cc::Build::new()
            .compiler("xtensa-esp32s3-elf-gcc")
            .file("../../third_party/esp_simd/src/vector/vector_i16/simd_fill_i16.S")
            .file("../../third_party/esp_simd/src/vector/vector_i16/simd_zeros_i16.S")
            .file("../../third_party/esp_simd/src/vector/vector_i16/simd_copy_i16.S")
            .compile("esp_simd");
    }

    println!("cargo::rerun-if-changed=build.rs");
}
