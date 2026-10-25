use std::io::Cursor;

use image::{DynamicImage, GrayImage, ImageFormat, Rgb, RgbImage, RgbaImage};
use qrcode::{EcLevel, QrCode};

use crate::error::{DiscoveryError, Result};

pub const DEFAULT_QR_SIZE: u32 = 640;
pub const DEFAULT_MAX_DIMENSION: u32 = 4096;
pub const LOGO_MAX_RATIO: u32 = 5;

pub fn render_png(wire: &str) -> Result<Vec<u8>> {
    render_colored(
        wire,
        Rgb([0, 0, 0]),
        Rgb([255, 255, 255]),
        DEFAULT_QR_SIZE,
        EcLevel::M,
    )
}

pub fn render_colored(
    wire: &str,
    dark: Rgb<u8>,
    light: Rgb<u8>,
    min_size: u32,
    ec: EcLevel,
) -> Result<Vec<u8>> {
    let image = render_rgb(wire, dark, light, min_size, ec)?;
    encode_png(&image)
}

pub fn render_png_with_logo(
    wire: &str,
    dark: Rgb<u8>,
    light: Rgb<u8>,
    min_size: u32,
    logo: Option<&RgbaImage>,
) -> Result<Vec<u8>> {
    let mut image = render_rgb(wire, dark, light, min_size, EcLevel::H)?;
    if let Some(logo) = logo {
        overlay_logo(&mut image, logo);
    }
    encode_png(&image)
}

fn render_rgb(
    wire: &str,
    dark: Rgb<u8>,
    light: Rgb<u8>,
    min_size: u32,
    ec: EcLevel,
) -> Result<RgbImage> {
    let code = QrCode::with_error_correction_level(wire.as_bytes(), ec)
        .map_err(|_| DiscoveryError::InvalidArgument("qr encode"))?;
    Ok(code
        .render::<Rgb<u8>>()
        .quiet_zone(true)
        .min_dimensions(min_size, min_size)
        .dark_color(dark)
        .light_color(light)
        .build())
}

fn encode_png(image: &RgbImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image.clone())
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|_| DiscoveryError::InvalidArgument("png encode"))?;
    Ok(out.into_inner())
}

fn overlay_logo(image: &mut RgbImage, logo: &RgbaImage) {
    let side = (image.width() / LOGO_MAX_RATIO).max(1);
    let pad = (side / 10).max(1);
    let resized = image::imageops::resize(logo, side, side, image::imageops::FilterType::Lanczos3);
    let logo_rgb = flatten_on_white(&resized);
    let outer = side + pad * 2;
    let x0 = (image.width().saturating_sub(outer)) / 2;
    let y0 = (image.height().saturating_sub(outer)) / 2;
    let white = Rgb([255, 255, 255]);
    for y in y0..y0 + outer {
        for x in x0..x0 + outer {
            image.put_pixel(x, y, white);
        }
    }
    image::imageops::overlay(image, &logo_rgb, i64::from(x0 + pad), i64::from(y0 + pad));
}

fn flatten_on_white(logo: &RgbaImage) -> RgbImage {
    let mut out = RgbImage::from_pixel(logo.width(), logo.height(), Rgb([255, 255, 255]));
    for (x, y, pixel) in logo.enumerate_pixels() {
        let alpha = u32::from(pixel[3]);
        let blend = |channel: u8| -> u8 {
            ((u32::from(channel) * alpha + 255 * (255 - alpha)) / 255) as u8
        };
        out.put_pixel(
            x,
            y,
            Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
        );
    }
    out
}

/// 返回图中所有可解码的二维码文本。空 Vec 表示画面里没有可读码，
/// 由调用方区分"空结果"与"参数错误"。
pub fn decode_strings(bytes: &[u8], max_dimension: u32) -> Result<Vec<String>> {
    decode_strings_with_pixels(bytes, max_dimension, u64::MAX)
}

pub fn decode_strings_with_pixels(
    bytes: &[u8],
    max_dimension: u32,
    max_pixels: u64,
) -> Result<Vec<String>> {
    let image = image::load_from_memory(bytes)
        .map_err(|_| DiscoveryError::InvalidArgument("image format"))?;
    if image.width() > max_dimension || image.height() > max_dimension {
        return Err(DiscoveryError::InvalidArgument("image too large"));
    }
    if u64::from(image.width()) * u64::from(image.height()) > max_pixels {
        return Err(DiscoveryError::InvalidArgument("image pixel budget"));
    }
    let gray: GrayImage = image.to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(gray);
    let grids = prepared.detect_grids();
    let mut found = Vec::new();
    for grid in grids {
        if let Ok((_, content)) = grid.decode() {
            found.push(content);
        }
    }
    Ok(found)
}
