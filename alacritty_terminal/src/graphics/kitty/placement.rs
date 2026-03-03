//! Kitty graphics image placement and ID resolution.

use crate::event::EventListener;
use crate::graphics::kitty::state::KittyState;
use crate::graphics::kitty_parser::KittyCommand;
use crate::graphics::{ColorType, GraphicData};
use crate::term::Term;

/// Place an image on the terminal grid.
pub fn place_image<L: EventListener>(
    term: &mut Term<L>,
    image_id: u32,
    cmd: &KittyCommand,
) -> Result<(), String> {
    let stored = term
        .graphics
        .kitty_state
        .get_image(image_id)
        .ok_or_else(|| format!("image id={image_id} not in storage"))?;

    let src_data = &stored.data;

    let (pixels, width, height, color_type) = if cmd.src_x != 0
        || cmd.src_y != 0
        || (cmd.src_w != 0 && cmd.src_w != src_data.width as u32)
        || (cmd.src_h != 0 && cmd.src_h != src_data.height as u32)
    {
        crop_image(src_data, cmd)?
    } else {
        (
            src_data.pixels.clone(),
            src_data.width,
            src_data.height,
            src_data.color_type,
        )
    };

    // Scale the image if columns/rows are specified.
    let (pixels, width, height) = if cmd.columns > 0 || cmd.rows > 0 {
        let params = ScaleParams {
            color_type,
            columns: cmd.columns,
            rows: cmd.rows,
            cell_width: term.graphics.cell_width as usize,
            cell_height: term.graphics.cell_height as usize,
        };
        scale_image(&pixels, width, height, &params)?
    } else {
        (pixels, width, height)
    };

    // Apply sub-cell pixel offsets via transparent padding.
    let (pixels, width, height, color_type) =
        apply_pixel_offsets(pixels, width, height, color_type, cmd.offset_x, cmd.offset_y);

    let graphic_id = term.graphics.next_id();

    let graphic = GraphicData {
        id: graphic_id,
        width,
        height,
        color_type,
        pixels,
        is_opaque: color_type == ColorType::Rgb,
    };

    let no_cursor_move = cmd.cursor_movement == 1;
    let saved_cursor = if no_cursor_move {
        Some(term.grid().cursor.point)
    } else {
        None
    };

    crate::graphics::insert_graphic(term, graphic, None);

    if let Some(point) = saved_cursor {
        term.grid_mut().cursor.point = point;
    }

    Ok(())
}

/// Calculate target dimensions for cell-based scaling.
///
/// Returns `(target_width, target_height)` in pixels.
pub fn calculate_scaled_dimensions(
    src_w: usize,
    src_h: usize,
    columns: u32,
    rows: u32,
    cell_width: usize,
    cell_height: usize,
) -> (usize, usize) {
    match (columns > 0, rows > 0) {
        (true, true) => {
            // Both specified: exact target size.
            let tw = columns as usize * cell_width;
            let th = rows as usize * cell_height;
            (tw, th)
        },
        (true, false) => {
            // Only columns: scale height proportionally.
            let tw = columns as usize * cell_width;
            let th = (src_h * tw).checked_div(src_w).unwrap_or(src_h);
            (tw, th)
        },
        (false, true) => {
            // Only rows: scale width proportionally.
            let th = rows as usize * cell_height;
            let tw = (src_w * th).checked_div(src_h).unwrap_or(src_w);
            (tw, th)
        },
        (false, false) => {
            // No scaling requested.
            (src_w, src_h)
        },
    }
}

/// Parameters for cell-based image scaling.
pub struct ScaleParams {
    /// Source image color type.
    pub color_type: ColorType,
    /// Display columns (`c=`).
    pub columns: u32,
    /// Display rows (`r=`).
    pub rows: u32,
    /// Terminal cell width in pixels.
    pub cell_width: usize,
    /// Terminal cell height in pixels.
    pub cell_height: usize,
}

/// Scale image pixel data to the target dimensions derived from columns/rows.
///
/// Uses bilinear (Triangle) filtering via the `image` crate.
pub fn scale_image(
    pixels: &[u8],
    src_w: usize,
    src_h: usize,
    params: &ScaleParams,
) -> Result<(Vec<u8>, usize, usize), String> {
    let (target_w, target_h) = calculate_scaled_dimensions(
        src_w,
        src_h,
        params.columns,
        params.rows,
        params.cell_width,
        params.cell_height,
    );

    if target_w == 0 || target_h == 0 {
        return Err("scaled dimensions are zero".into());
    }

    // If the dimensions haven't changed, skip the resize.
    if target_w == src_w && target_h == src_h {
        return Ok((pixels.to_vec(), src_w, src_h));
    }

    // Convert to RGBA for the image crate.
    let rgba_pixels = match params.color_type {
        ColorType::Rgba => pixels.to_vec(),
        ColorType::Rgb => rgb_to_rgba(pixels),
    };

    let src_image = image::RgbaImage::from_raw(src_w as u32, src_h as u32, rgba_pixels)
        .ok_or_else(|| {
            format!(
                "failed to create image buffer from {src_w}x{src_h} ({} bytes)",
                pixels.len()
            )
        })?;

    let resized = image::imageops::resize(
        &src_image,
        target_w as u32,
        target_h as u32,
        image::imageops::FilterType::Triangle,
    );

    // Always output RGBA after scaling (the resize may introduce alpha even
    // on originally-RGB images due to filtering at edges).
    Ok((resized.into_raw(), target_w, target_h))
}

/// Apply sub-cell pixel offsets by padding the image with transparent pixels.
///
/// If both offsets are zero, the data is returned unchanged (possibly
/// promoting RGB → RGBA when offsets are applied).
pub fn apply_pixel_offsets(
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    color_type: ColorType,
    offset_x: u32,
    offset_y: u32,
) -> (Vec<u8>, usize, usize, ColorType) {
    let ox = offset_x as usize;
    let oy = offset_y as usize;

    if ox == 0 && oy == 0 {
        return (pixels, width, height, color_type);
    }

    let new_w = width + ox;
    let new_h = height + oy;
    let new_stride = new_w * 4;

    // We need RGBA to have transparent padding.
    let rgba_pixels = match color_type {
        ColorType::Rgba => pixels,
        ColorType::Rgb => rgb_to_rgba(&pixels),
    };

    let src_stride = width * 4;
    let mut out = vec![0u8; new_h * new_stride];

    for row in 0..height {
        let src_start = row * src_stride;
        let dst_start = (row + oy) * new_stride + ox * 4;
        out[dst_start..dst_start + src_stride]
            .copy_from_slice(&rgba_pixels[src_start..src_start + src_stride]);
    }

    (out, new_w, new_h, ColorType::Rgba)
}

/// Convert RGB pixel data to RGBA (fully opaque).
fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let pixel_count = rgb.len() / 3;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for chunk in rgb.chunks_exact(3) {
        rgba.extend_from_slice(chunk);
        rgba.push(255);
    }
    rgba
}

/// Crop an image according to source rectangle parameters.
pub fn crop_image(
    src: &GraphicData,
    cmd: &KittyCommand,
) -> Result<(Vec<u8>, usize, usize, ColorType), String> {
    let bpp = match src.color_type {
        ColorType::Rgba => 4,
        ColorType::Rgb => 3,
    };

    let sx = cmd.src_x as usize;
    let sy = cmd.src_y as usize;
    let sw = if cmd.src_w == 0 { src.width - sx } else { cmd.src_w as usize };
    let sh = if cmd.src_h == 0 { src.height - sy } else { cmd.src_h as usize };

    if sx + sw > src.width || sy + sh > src.height {
        return Err(format!(
            "source rect ({sx},{sy},{sw},{sh}) exceeds image ({}x{})",
            src.width, src.height
        ));
    }

    if sw == 0 || sh == 0 {
        return Err("source rect has zero width or height".into());
    }

    let src_stride = src.width * bpp;
    let dst_stride = sw * bpp;
    let mut cropped = Vec::with_capacity(sh * dst_stride);

    for row in sy..sy + sh {
        let src_start = row * src_stride + sx * bpp;
        let src_end = src_start + dst_stride;
        cropped.extend_from_slice(&src.pixels[src_start..src_end]);
    }

    Ok((cropped, sw, sh, src.color_type))
}

/// Resolve or auto-assign an image ID for a transmit command.
pub fn resolve_or_assign_id<L: EventListener>(term: &mut Term<L>, cmd: &KittyCommand) -> u32 {
    let state = &mut term.graphics.kitty_state;

    let image_id = if cmd.image_id != 0 {
        cmd.image_id
    } else {
        state.next_id()
    };

    if cmd.image_number != 0 {
        state.number_to_id.insert(cmd.image_number, image_id);
    }

    image_id
}

/// Resolve an image ID from a command, checking both `i=` and `I=`.
pub fn resolve_image_id(state: &KittyState, cmd: &KittyCommand) -> Option<u32> {
    if cmd.image_id != 0 {
        Some(cmd.image_id)
    } else if cmd.image_number != 0 {
        state.resolve_number(cmd.image_number)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::GraphicId;

    // ---------------------------------------------------------------
    // Dimension calculation tests
    // ---------------------------------------------------------------

    #[test]
    fn scale_dimensions_both_set() {
        let (tw, th) = calculate_scaled_dimensions(100, 200, 5, 3, 8, 16);
        assert_eq!(tw, 40); // 5 * 8
        assert_eq!(th, 48); // 3 * 16
    }

    #[test]
    fn scale_dimensions_only_columns() {
        // 100×200, columns=5, cell_width=8 → tw=40, th = 200*40/100 = 80
        let (tw, th) = calculate_scaled_dimensions(100, 200, 5, 0, 8, 16);
        assert_eq!(tw, 40);
        assert_eq!(th, 80);
    }

    #[test]
    fn scale_dimensions_only_rows() {
        // 100×200, rows=4, cell_height=16 → th=64, tw = 100*64/200 = 32
        let (tw, th) = calculate_scaled_dimensions(100, 200, 0, 4, 8, 16);
        assert_eq!(tw, 32);
        assert_eq!(th, 64);
    }

    #[test]
    fn scale_dimensions_none() {
        let (tw, th) = calculate_scaled_dimensions(100, 200, 0, 0, 8, 16);
        assert_eq!(tw, 100);
        assert_eq!(th, 200);
    }

    #[test]
    fn scale_dimensions_square_proportional() {
        // 64×64, columns=4, cell_width=10 → tw=40, th=64*40/64=40
        let (tw, th) = calculate_scaled_dimensions(64, 64, 4, 0, 10, 10);
        assert_eq!(tw, 40);
        assert_eq!(th, 40);
    }

    // ---------------------------------------------------------------
    // Pixel scaling tests
    // ---------------------------------------------------------------

    fn make_scale_params(color_type: ColorType, columns: u32, rows: u32, cw: usize, ch: usize) -> ScaleParams {
        ScaleParams { color_type, columns, rows, cell_width: cw, cell_height: ch }
    }

    #[test]
    fn scale_image_upscale() {
        // 4×4 solid red RGBA → scale to 8×8
        let red = [255u8, 0, 0, 255];
        let pixels: Vec<u8> = red.repeat(4 * 4);
        let params = make_scale_params(ColorType::Rgba, 1, 1, 8, 8);
        let (out, w, h) = scale_image(&pixels, 4, 4, &params).unwrap();
        assert_eq!(w, 8);
        assert_eq!(h, 8);
        assert_eq!(out.len(), 8 * 8 * 4);
        // All pixels should still be red (solid color scales perfectly).
        for chunk in out.chunks_exact(4) {
            assert_eq!(chunk, &red);
        }
    }

    #[test]
    fn scale_image_downscale() {
        // 8×8 solid green → scale to 4×4
        let green = [0u8, 255, 0, 255];
        let pixels: Vec<u8> = green.repeat(8 * 8);
        let params = make_scale_params(ColorType::Rgba, 2, 2, 2, 2);
        let (out, w, h) = scale_image(&pixels, 8, 8, &params).unwrap();
        assert_eq!(w, 4);
        assert_eq!(h, 4);
        assert_eq!(out.len(), 4 * 4 * 4);
    }

    #[test]
    fn scale_image_noop_when_same_size() {
        let pixels: Vec<u8> = vec![128; 4 * 4 * 4];
        let params = make_scale_params(ColorType::Rgba, 2, 2, 2, 2);
        let (out, w, h) = scale_image(&pixels, 4, 4, &params).unwrap();
        assert_eq!(w, 4);
        assert_eq!(h, 4);
        assert_eq!(out, pixels);
    }

    #[test]
    fn scale_image_rgb_input() {
        // Ensure RGB input is handled (promoted to RGBA for scaling).
        let blue = [0u8, 0, 255];
        let pixels: Vec<u8> = blue.repeat(4 * 4);
        let params = make_scale_params(ColorType::Rgb, 1, 1, 8, 8);
        let (out, w, h) = scale_image(&pixels, 4, 4, &params).unwrap();
        assert_eq!(w, 8);
        assert_eq!(h, 8);
        // Output is RGBA (4 bytes per pixel).
        assert_eq!(out.len(), 8 * 8 * 4);
    }

    #[test]
    fn scale_image_zero_target_errors() {
        let pixels: Vec<u8> = vec![0; 16];
        // columns=1 but cell_width=0 → target 0.
        let params = make_scale_params(ColorType::Rgba, 1, 0, 0, 16);
        let result = scale_image(&pixels, 2, 2, &params);
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // Crop + scale interaction
    // ---------------------------------------------------------------

    #[test]
    fn crop_then_scale() {
        // 8×8 image, crop to 4×4, then scale to 2 columns × 2 rows at 4×4 cell → 8×8.
        let pixels: Vec<u8> = vec![100; 8 * 8 * 4];
        let src = GraphicData {
            id: GraphicId(0),
            width: 8,
            height: 8,
            color_type: ColorType::Rgba,
            pixels,
            is_opaque: false,
        };
        let cmd = KittyCommand {
            src_x: 2,
            src_y: 2,
            src_w: 4,
            src_h: 4,
            ..Default::default()
        };
        let (cropped, cw, ch, ct) = crop_image(&src, &cmd).unwrap();
        assert_eq!((cw, ch), (4, 4));

        let params = make_scale_params(ct, 2, 2, 4, 4);
        let (scaled, sw, sh) = scale_image(&cropped, cw, ch, &params).unwrap();
        assert_eq!(sw, 8);
        assert_eq!(sh, 8);
        assert_eq!(scaled.len(), 8 * 8 * 4);
    }

    // ---------------------------------------------------------------
    // Pixel offset (padding) tests
    // ---------------------------------------------------------------

    #[test]
    fn offset_no_op() {
        let pixels = vec![255u8; 2 * 2 * 4];
        let (out, w, h, ct) = apply_pixel_offsets(pixels.clone(), 2, 2, ColorType::Rgba, 0, 0);
        assert_eq!(w, 2);
        assert_eq!(h, 2);
        assert_eq!(ct, ColorType::Rgba);
        assert_eq!(out, pixels);
    }

    #[test]
    fn offset_x_only() {
        // 2×2 white RGBA, offset_x=3 → 5×2 with 3-pixel transparent left strip.
        let white = [255u8, 255, 255, 255];
        let pixels: Vec<u8> = white.repeat(2 * 2);
        let (out, w, h, ct) = apply_pixel_offsets(pixels, 2, 2, ColorType::Rgba, 3, 0);
        assert_eq!(w, 5);
        assert_eq!(h, 2);
        assert_eq!(ct, ColorType::Rgba);
        assert_eq!(out.len(), 5 * 2 * 4);

        // First 3 pixels of row 0 should be transparent (all zeroes).
        for px in 0..3 {
            let start = px * 4;
            assert_eq!(&out[start..start + 4], &[0, 0, 0, 0]);
        }
        // Pixel at (3,0) should be white.
        assert_eq!(&out[12..16], &white);
    }

    #[test]
    fn offset_y_only() {
        // 2×2 red RGBA, offset_y=2 → 2×4 with 2 transparent rows on top.
        let red = [255u8, 0, 0, 255];
        let pixels: Vec<u8> = red.repeat(2 * 2);
        let (out, w, h, ct) = apply_pixel_offsets(pixels, 2, 2, ColorType::Rgba, 0, 2);
        assert_eq!(w, 2);
        assert_eq!(h, 4);
        assert_eq!(ct, ColorType::Rgba);

        // First 2 rows (2*2*4 = 16 bytes) should be all zeroes.
        assert!(out[..16].iter().all(|&b| b == 0));
        // Row 2, pixel 0 should be red.
        assert_eq!(&out[16..20], &red);
    }

    #[test]
    fn offset_both() {
        let blue = [0u8, 0, 255, 255];
        let pixels: Vec<u8> = blue.repeat(1); // 1×1 image
        let (out, w, h, ct) = apply_pixel_offsets(pixels, 1, 1, ColorType::Rgba, 2, 3);
        assert_eq!(w, 3);
        assert_eq!(h, 4);
        assert_eq!(ct, ColorType::Rgba);

        // Only pixel at (2, 3) should be blue; everything else transparent.
        for row in 0..h {
            for col in 0..w {
                let start = (row * w + col) * 4;
                if row == 3 && col == 2 {
                    assert_eq!(&out[start..start + 4], &blue);
                } else {
                    assert_eq!(&out[start..start + 4], &[0, 0, 0, 0]);
                }
            }
        }
    }

    #[test]
    fn offset_promotes_rgb_to_rgba() {
        let pixels = vec![128u8; 2 * 2 * 3]; // RGB
        let (out, w, h, ct) = apply_pixel_offsets(pixels, 2, 2, ColorType::Rgb, 1, 0);
        assert_eq!(w, 3);
        assert_eq!(h, 2);
        assert_eq!(ct, ColorType::Rgba);
        assert_eq!(out.len(), 3 * 2 * 4);
    }

    // ---------------------------------------------------------------
    // rgb_to_rgba helper
    // ---------------------------------------------------------------

    #[test]
    fn rgb_to_rgba_conversion() {
        let rgb = vec![10, 20, 30, 40, 50, 60];
        let rgba = rgb_to_rgba(&rgb);
        assert_eq!(rgba, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    }

    // ---------------------------------------------------------------
    // Existing crop tests (must keep passing)
    // ---------------------------------------------------------------

    #[test]
    fn crop_image_basic() {
        let pixels: Vec<u8> = [255, 0, 0, 255].repeat(16);
        let src = GraphicData {
            id: GraphicId(0),
            width: 4,
            height: 4,
            color_type: ColorType::Rgba,
            pixels,
            is_opaque: false,
        };
        let cmd = KittyCommand {
            src_x: 1,
            src_y: 1,
            src_w: 2,
            src_h: 2,
            ..Default::default()
        };
        let (cropped, w, h, ct) = crop_image(&src, &cmd).unwrap();
        assert_eq!(w, 2);
        assert_eq!(h, 2);
        assert_eq!(ct, ColorType::Rgba);
        assert_eq!(cropped.len(), 2 * 2 * 4);
    }

    #[test]
    fn crop_image_out_of_bounds() {
        let src = GraphicData {
            id: GraphicId(0),
            width: 4,
            height: 4,
            color_type: ColorType::Rgba,
            pixels: vec![0; 64],
            is_opaque: false,
        };
        let cmd = KittyCommand {
            src_x: 3,
            src_y: 3,
            src_w: 3,
            src_h: 3,
            ..Default::default()
        };
        assert!(crop_image(&src, &cmd).is_err());
    }
}