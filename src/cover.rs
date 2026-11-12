use crate::model::Track;
use image::{imageops::FilterType, DynamicImage};
use lofty::prelude::*;

pub const COVER_N: u32 = 14; // the cover is drawn as a 14 x 14 grid of blocks

/// The cover picture of a song: the downloaded thumbnail, or the cover inside a saved file.
pub fn load_image(track: &Track) -> Option<DynamicImage> {
    if let Some(path) = track.id.strip_prefix("local:") {
        let file = lofty::read_from_path(path).ok()?;
        let tag = file.primary_tag().or_else(|| file.first_tag())?;
        let pic = tag.pictures().first()?;
        return image::load_from_memory(pic.data()).ok();
    }
    if track.thumb.is_empty() {
        return None;
    }
    let bytes = crate::ytm::http_bytes(&track.thumb).ok()?;
    image::load_from_memory(&bytes).ok()
}

pub fn square(img: &DynamicImage) -> DynamicImage {
    let side = img.width().min(img.height());
    img.crop_imm((img.width() - side) / 2, (img.height() - side) / 2, side, side)
}

/// COVER_N*COVER_N colours. mode 'blue' = black-to-accent-to-white shades, 'color' = real colours.
pub fn mosaic(img: &DynamicImage, mode: &str, accent: [u8; 3]) -> Vec<[u8; 3]> {
    let small = image::imageops::resize(&square(img).to_rgb8(), COVER_N, COVER_N, FilterType::Lanczos3);
    if mode == "color" {
        return small.pixels().map(|p| p.0).collect();
    }
    let grays: Vec<f32> = small.pixels().map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).collect();
    let lo = grays.iter().cloned().fold(f32::MAX, f32::min);
    let hi = grays.iter().cloned().fold(f32::MIN, f32::max);
    let a = accent.map(|c| c as f32);
    grays.iter().map(|&v| {
        let t = if hi > lo { (v - lo) / (hi - lo) } else { 0.5 };
        let mut out = [0u8; 3];
        for i in 0..3 {
            out[i] = if t < 0.5 { a[i] * t * 2.0 } else { a[i] + (255.0 - a[i]) * (t - 0.5) * 2.0 } as u8;
        }
        out
    }).collect()
}
