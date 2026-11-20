//! Visible watermark: rasterised text with optional outline, tiled or single,
//! rotated by an arbitrary angle and alpha-blended onto the image.

use ab_glyph::{point, Font, FontRef, Glyph, OutlinedGlyph, PxScale, ScaleFont};
use clap::ValueEnum;

const FONT_DATA: &[u8] = include_bytes!("../assets/Roboto-Bold.ttf");

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Position {
    Center,
    TopLeft,
    Top,
    TopRight,
    Left,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

#[derive(Clone, Copy)]
pub struct Style<'a> {
    pub text: &'a str,
    pub angle_deg: f32,
    pub opacity: f32,
    pub size_pct: f32,
    pub gap_pct: f32,
    pub single: bool,
    pub position: Position,
    pub color: [u8; 3],
    pub outline: bool,
}

struct Mask {
    w: usize,
    h: usize,
    line_h: f32,
    fill: Vec<u8>,
    outline: Vec<u8>,
}

pub fn apply(buffer: &mut [u8], width: usize, height: usize, channels: usize, style: &Style) {
    if style.text.trim().is_empty() || style.opacity <= 0.0 || width == 0 || height == 0 {
        return;
    }
    let min_dim = width.min(height) as f32;
    let px = (style.size_pct / 100.0 * min_dim).clamp(8.0, min_dim * 0.45);
    let outline_px = if style.outline {
        (px * 0.055).clamp(1.0, 8.0)
    } else {
        0.0
    };
    let mask = build_mask(style.text, px, outline_px);
    if mask.w == 0 || mask.h == 0 {
        return;
    }

    let opacity = style.opacity.clamp(0.0, 1.0);
    let fill = [
        style.color[0] as f32,
        style.color[1] as f32,
        style.color[2] as f32,
    ];
    let outline_color = [0.0f32, 0.0, 0.0];

    let theta = style.angle_deg.to_radians();
    let (sin, cos) = theta.sin_cos();

    let gap = (style.gap_pct / 100.0 * mask.line_h).clamp(2.0, min_dim);
    let tile_w = mask.w as f32 + gap;
    let tile_h = mask.h as f32 + gap;

    let (anchor_x, anchor_y) = if style.single {
        anchor(
            width as f32,
            height as f32,
            &mask,
            style.position,
            cos,
            sin,
        )
    } else {
        (0.0, 0.0)
    };

    for y in 0..height {
        let fy = y as f32 + 0.5;
        for x in 0..width {
            let fx = x as f32 + 0.5;

            let (u, v) = if style.single {
                let du = fx - anchor_x;
                let dv = fy - anchor_y;
                (
                    cos * du + sin * dv + mask.w as f32 * 0.5,
                    -sin * du + cos * dv + mask.h as f32 * 0.5,
                )
            } else {
                let u = (cos * fx + sin * fy).rem_euclid(tile_w);
                let v = (-sin * fx + cos * fy).rem_euclid(tile_h);
                if u >= mask.w as f32 || v >= mask.h as f32 {
                    continue;
                }
                (u, v)
            };

            if style.single
                && (u < 0.0 || v < 0.0 || u >= mask.w as f32 || v >= mask.h as f32)
            {
                continue;
            }

            let fill_cov = sample(&mask.fill, mask.w, mask.h, u, v, !style.single);
            if fill_cov <= 0.001 && !style.outline {
                continue;
            }
            let out_cov = if style.outline {
                sample(&mask.outline, mask.w, mask.h, u, v, !style.single)
            } else {
                0.0
            };

            let fa = fill_cov * opacity;
            let oa = out_cov * opacity * (1.0 - fill_cov);
            let alpha = fa + oa;
            if alpha <= 0.001 {
                continue;
            }

            let base = (y * width + x) * channels;
            let r = fill[0] * fa + outline_color[0] * oa;
            let g = fill[1] * fa + outline_color[1] * oa;
            let b = fill[2] * fa + outline_color[2] * oa;
            let inv = 1.0 - alpha;

            buffer[base] = (buffer[base] as f32 * inv + r).round().clamp(0.0, 255.0) as u8;
            buffer[base + 1] =
                (buffer[base + 1] as f32 * inv + g).round().clamp(0.0, 255.0) as u8;
            buffer[base + 2] =
                (buffer[base + 2] as f32 * inv + b).round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn anchor(
    width: f32,
    height: f32,
    mask: &Mask,
    position: Position,
    cos: f32,
    sin: f32,
) -> (f32, f32) {
    // Rotated footprint of the mask, so single watermarks stay inside the frame.
    let box_w = cos.abs() * mask.w as f32 + sin.abs() * mask.h as f32;
    let box_h = sin.abs() * mask.w as f32 + cos.abs() * mask.h as f32;
    let margin = 0.03 * width.min(height);
    let cx = width * 0.5;
    let cy = height * 0.5;
    let left = margin + box_w * 0.5;
    let right = width - margin - box_w * 0.5;
    let top = margin + box_h * 0.5;
    let bottom = height - margin - box_h * 0.5;
    match position {
        Position::Center => (cx, cy),
        Position::TopLeft => (left, top),
        Position::Top => (cx, top),
        Position::TopRight => (right, top),
        Position::Left => (left, cy),
        Position::Right => (right, cy),
        Position::BottomLeft => (left, bottom),
        Position::Bottom => (cx, bottom),
        Position::BottomRight => (right, bottom),
    }
}

fn build_mask(text: &str, px: f32, outline_px: f32) -> Mask {
    let font = FontRef::try_from_slice(FONT_DATA).expect("embedded font is valid");
    let scale = PxScale::from(px);
    let scaled = font.as_scaled(scale);
    let ascent = scaled.ascent();
    let line_h = (ascent - scaled.descent() + scaled.line_gap()).max(px);
    let lines: Vec<&str> = text.split('\n').collect();

    let mut widths = Vec::with_capacity(lines.len());
    for line in &lines {
        let mut width = 0.0f32;
        let mut prev = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(previous) = prev {
                width += scaled.kern(previous, id);
            }
            width += scaled.h_advance(id);
            prev = Some(id);
        }
        widths.push(width);
    }
    let text_w = widths.iter().copied().fold(0.0f32, f32::max);
    if text_w <= 0.0 {
        return Mask {
            w: 0,
            h: 0,
            line_h,
            fill: Vec::new(),
            outline: Vec::new(),
        };
    }

    let pad = (outline_px + px * 0.12).ceil().max(2.0);
    let mask_w = (text_w + 2.0 * pad).ceil() as usize;
    let mask_h = (line_h * lines.len() as f32 + 2.0 * pad).ceil() as usize;
    let mut fill = vec![0u8; mask_w * mask_h];
    let mut outline = vec![0u8; mask_w * mask_h];

    for (line_index, line) in lines.iter().enumerate() {
        let baseline = pad + ascent + line_index as f32 * line_h;
        let mut cursor = pad + (text_w - widths[line_index]) * 0.5;
        let mut prev = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(previous) = prev {
                cursor += scaled.kern(previous, id);
            }
            let glyph = Glyph {
                id,
                scale,
                position: point(cursor, baseline),
            };
            if let Some(outlined) = font.outline_glyph(glyph) {
                draw_into(&outlined, &mut fill, mask_w, mask_h);
            }
            if outline_px > 0.0 {
                for ring in [1.0f32, 0.5] {
                    for step in 0..12 {
                        let angle = step as f32 * std::f32::consts::TAU / 12.0;
                        let dx = angle.cos() * outline_px * ring;
                        let dy = angle.sin() * outline_px * ring;
                        let offset_glyph = Glyph {
                            id,
                            scale,
                            position: point(cursor + dx, baseline + dy),
                        };
                        if let Some(outlined) = font.outline_glyph(offset_glyph) {
                            draw_into(&outlined, &mut outline, mask_w, mask_h);
                        }
                    }
                }
            }
            cursor += scaled.h_advance(id);
            prev = Some(id);
        }
    }

    Mask {
        w: mask_w,
        h: mask_h,
        line_h,
        fill,
        outline,
    }
}

fn draw_into(glyph: &OutlinedGlyph, target: &mut [u8], width: usize, height: usize) {
    let bounds = glyph.px_bounds();
    glyph.draw(|gx, gy, coverage| {
        let x = bounds.min.x as i32 + gx as i32;
        let y = bounds.min.y as i32 + gy as i32;
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= width || y >= height {
            return;
        }
        let value = (coverage * 255.0).round().clamp(0.0, 255.0) as u8;
        let slot = &mut target[y * width + x];
        if value > *slot {
            *slot = value;
        }
    });
}

fn sample(mask: &[u8], width: usize, height: usize, x: f32, y: f32, clamp_edges: bool) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let x0 = x0 as i32;
    let y0 = y0 as i32;

    let top = texel(mask, width, height, x0, y0, clamp_edges) * (1.0 - fx)
        + texel(mask, width, height, x0 + 1, y0, clamp_edges) * fx;
    let bottom = texel(mask, width, height, x0, y0 + 1, clamp_edges) * (1.0 - fx)
        + texel(mask, width, height, x0 + 1, y0 + 1, clamp_edges) * fx;
    top * (1.0 - fy) + bottom * fy
}

fn texel(mask: &[u8], width: usize, height: usize, x: i32, y: i32, clamp_edges: bool) -> f32 {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        if !clamp_edges {
            return 0.0;
        }
        let cx = x.clamp(0, width as i32 - 1) as usize;
        let cy = y.clamp(0, height as i32 - 1) as usize;
        return mask[cy * width + cx] as f32 / 255.0;
    }
    mask[y as usize * width + x as usize] as f32 / 255.0
}

pub fn parse_color(text: &str) -> Result<[u8; 3], String> {
    let name = text.trim().to_ascii_lowercase();
    let named = match name.as_str() {
        "white" => Some([255, 255, 255]),
        "black" => Some([0, 0, 0]),
        "red" => Some([220, 40, 40]),
        "gray" | "grey" => Some([128, 128, 128]),
        "yellow" => Some([250, 220, 40]),
        "blue" => Some([40, 90, 220]),
        _ => None,
    };
    if let Some(rgb) = named {
        return Ok(rgb);
    }
    let hex = name.strip_prefix('#').unwrap_or(&name);
    if hex.len() == 6 {
        if let Ok(v) = u32::from_str_radix(hex, 16) {
            return Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8]);
        }
    }
    Err(format!(
        "invalid color '{text}' (use #RRGGBB or white/black/red/gray/yellow/blue)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(text: &str, opacity: f32) -> Style<'_> {
        Style {
            text,
            angle_deg: 0.0,
            opacity,
            size_pct: 20.0,
            gap_pct: 200.0,
            single: true,
            position: Position::Center,
            color: [255, 255, 255],
            outline: false,
        }
    }

    #[test]
    fn opacity_is_respected() {
        let (width, height) = (200usize, 100usize);

        // Background 100 + half-opacity white text: never brighter than ~178.
        let mut buffer = vec![100u8; width * height * 3];
        apply(&mut buffer, width, height, 3, &style("HHH", 0.5));
        let max = *buffer.iter().max().unwrap();
        assert!(max <= 180, "50% opacity produced {max}, expected <= 180");
        assert!(max >= 150, "text disappeared at 50% opacity (max {max})");

        // Fully opaque white text must reach 255.
        let mut buffer = vec![100u8; width * height * 3];
        apply(&mut buffer, width, height, 3, &style("HHH", 1.0));
        assert_eq!(*buffer.iter().max().unwrap(), 255);
    }
}
