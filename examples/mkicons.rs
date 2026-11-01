//! Regenerate the hicolor icon sizes and the embedded 256 px icon from the
//! master artwork in `packaging/ferris-browser.png`.
//!
//!     cargo run --release --example mkicons

use image::imageops::FilterType;

fn main() {
    let img = image::open(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/packaging/ferris-browser.png"
    ))
    .expect("packaging/ferris-browser.png missing");
    for size in [64u32, 128, 256, 512, 1024] {
        let out = img.resize_exact(size, size, FilterType::Triangle);
        let dir = format!(
            "{}/packaging/hicolor/{size}x{size}/apps",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::create_dir_all(&dir).expect("mkdir");
        out.save(format!("{dir}/ferris-browser.png")).expect("save");
    }
    img.resize_exact(256, 256, FilterType::Triangle)
        .save(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/packaging/icon-256.png"
        ))
        .expect("save embedded icon");
    println!("icons written to packaging/hicolor/ and packaging/icon-256.png");
}
