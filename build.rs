//! Build script. Generates the multi-size Windows `.ico` from the packaged
//! PNG on every build, so the icon has a single source of truth, and embeds
//! it into the executable's resources on Windows.

use std::env;
use std::io::Cursor;
use std::path::Path;

/// Sizes stored as uncompressed BMP entries; Windows documents PNG-compressed
/// ICO entries only for 256x256.
const BMP_SIZES: [u32; 4] = [16, 24, 32, 48];
const PNG_SIZES: [u32; 3] = [64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=packaging/itmr-dragonfable-launcher.png");
    // Generated on every platform so icon regressions fail every build,
    // not just Windows CI.
    let icon = windows_icon();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let path = Path::new(&env::var("OUT_DIR").expect("OUT_DIR must be set")).join("icon.ico");
    std::fs::write(&path, &icon).expect("failed to write generated icon");
    winresource::WindowsResource::new()
        .set_icon(path.to_str().expect("OUT_DIR path must be valid UTF-8"))
        .compile()
        .expect("failed to embed the Windows icon resource");
}

fn windows_icon() -> Vec<u8> {
    let source = image::load_from_memory(include_bytes!(
        "packaging/itmr-dragonfable-launcher.png",
    ))
        .expect("bundled icon must be decodable")
        .to_rgba8();
    let resized = |size: u32| {
        image::imageops::resize(&source, size, size, image::imageops::FilterType::Lanczos3)
    };
    let mut entries: Vec<(u32, Vec<u8>)> = BMP_SIZES
        .map(|size| (size, bmp_entry(&resized(size))))
        .into_iter()
        .collect();
    entries.extend(PNG_SIZES.map(|size| {
        let mut png = Vec::new();
        resized(size)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("PNG encoding cannot fail for in-memory RGBA data");
        (size, png)
    }));
    assemble_ico(&entries)
}

/// One ICO directory entry with uncompressed 32-bit BGRA pixel data:
/// BITMAPINFOHEADER (with the height doubled to account for the AND mask),
/// bottom-up BGRA pixels, then an all-opaque transparency mask.
fn bmp_entry(image: &image::RgbaImage) -> Vec<u8> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let mut pixels = Vec::with_capacity(width * height * 4);
    for y in (0..height).rev() {
        for x in 0..width {
            let [r, g, b, a] = image.get_pixel(x as u32, y as u32).0;
            pixels.extend_from_slice(&[b, g, r, a]);
        }
    }
    let mask_len = ((width + 31) / 32) * 4 * height;
    const HEADER_SIZE: u32 = 40;
    let mut entry = Vec::with_capacity(HEADER_SIZE as usize + pixels.len() + mask_len);
    entry.extend_from_slice(&HEADER_SIZE.to_le_bytes());
    entry.extend_from_slice(&(width as i32).to_le_bytes());
    entry.extend_from_slice(&((height * 2) as i32).to_le_bytes());
    entry.extend_from_slice(&1_u16.to_le_bytes());
    entry.extend_from_slice(&32_u16.to_le_bytes());
    entry.extend_from_slice(&0_u32.to_le_bytes());
    entry.extend_from_slice(&((pixels.len() + mask_len) as u32).to_le_bytes());
    entry.extend_from_slice(&0_i32.to_le_bytes());
    entry.extend_from_slice(&0_i32.to_le_bytes());
    entry.extend_from_slice(&0_u32.to_le_bytes());
    entry.extend_from_slice(&0_u32.to_le_bytes());
    entry.extend_from_slice(&pixels);
    entry.resize(entry.len() + mask_len, 0);
    entry
}

/// Wraps sized entries in the ICO container: ICONDIR header, one 16-byte
/// directory record per entry, then all payloads back to back.
fn assemble_ico(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut ico = Vec::new();
    ico.extend_from_slice(&0_u16.to_le_bytes());
    ico.extend_from_slice(&1_u16.to_le_bytes());
    ico.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = (6 + 16 * entries.len()) as u32;
    for &(size, ref blob) in entries {
        ico.extend_from_slice(&[(size & 0xFF) as u8, (size & 0xFF) as u8, 0, 0]);
        ico.extend_from_slice(&1_u16.to_le_bytes());
        ico.extend_from_slice(&32_u16.to_le_bytes());
        ico.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += blob.len() as u32;
    }
    for (_, blob) in entries {
        ico.extend_from_slice(blob);
    }
    ico
}
