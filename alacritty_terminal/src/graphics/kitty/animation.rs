//! Animation frame storage, composition, and control logic for the kitty graphics protocol.
//!
//! This module handles:
//! - `a=f` (TransmitFrame): adding/editing animation frames
//! - `a=a` (AnimationControl): controlling playback state
//! - `a=c` (ComposeFrames): compositing pixels between frames
//!
//! Key design: **lazy promotion** — single-frame images stay as plain `KittyImage`.
//! When a second frame arrives, the image is promoted to an `AnimationState`.
//!
//! See: <https://sw.kovidgoyal.net/kitty/graphics-protocol/#animation>

use log::debug;

use super::decode::decode_payload;
use super::state::KittyState;
use crate::graphics::kitty_parser::KittyCommand;
use crate::graphics::ColorType;

// ── Data Structures ────────────────────────────────────────────────────

/// Composition mode for animation frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionMode {
    /// Overwrite: new frame pixels replace old ones completely.
    Overwrite,
    /// AlphaBlend: composite new frame over old using standard alpha compositing.
    AlphaBlend,
}

/// A single animation frame.
#[derive(Debug, Clone)]
pub struct AnimationFrame {
    /// RGBA pixel data (always RGBA for animation).
    pub pixels: Vec<u8>,
    /// Frame width in pixels.
    pub width: usize,
    /// Frame height in pixels.
    pub height: usize,
    /// Frame duration in milliseconds (0 = use previous frame's duration).
    pub gap_ms: u32,
    /// How this frame composites over the previous.
    pub composition_mode: CompositionMode,
    /// Background color (RGBA packed as u32) to fill before compositing. 0 = transparent.
    pub background: u32,
}

/// Playback state for an animated image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Animation is stopped.
    Stopped,
    /// Animation is running.
    Running,
    /// Animation is loading frames (not yet playing).
    Loading,
}

/// Animation state for a multi-frame kitty image.
#[derive(Debug)]
pub struct AnimationState {
    /// All frames in order.
    pub frames: Vec<AnimationFrame>,
    /// Currently displayed frame index.
    pub current_frame: usize,
    /// Playback state.
    pub state: PlaybackState,
    /// Number of loops (0 = infinite).
    pub loops: u32,
    /// Loops completed so far.
    pub loops_done: u32,
}

impl AnimationState {
    /// Create a new animation from a single frame.
    pub fn new(first_frame: AnimationFrame) -> Self {
        AnimationState {
            frames: vec![first_frame],
            current_frame: 0,
            state: PlaybackState::Loading,
            loops: 0,
            loops_done: 0,
        }
    }

    /// Add a new frame or edit an existing one.
    ///
    /// If `edit_index` is `Some(idx)` and `idx` is within bounds, replace that frame.
    /// Otherwise, append the frame.
    pub fn add_frame(&mut self, frame: AnimationFrame, edit_index: Option<usize>) {
        match edit_index {
            Some(idx) if idx < self.frames.len() => {
                self.frames[idx] = frame;
            },
            _ => {
                self.frames.push(frame);
            },
        }
    }
}

// ── Core Pixel Composition ─────────────────────────────────────────────

/// Core pixel composition function.
///
/// Composites `src` pixels over `dst` pixels for a frame of the given dimensions.
/// Both `dst` and `src` are RGBA pixel data (4 bytes per pixel).
///
/// - If `background != 0`, fills `dst` with the background color first.
/// - `Overwrite` mode: source pixels completely replace destination pixels.
/// - `AlphaBlend` mode: standard "source over" alpha compositing.
pub fn blit(
    dst: &mut [u8],
    src: &[u8],
    width: usize,
    height: usize,
    mode: CompositionMode,
    background: u32,
) {
    let frame_bytes = width * height * 4;
    let dst_len = dst.len().min(frame_bytes);

    // Fill with background color if specified.
    if background != 0 {
        let bg = background_to_rgba(background);
        for pixel in dst[..dst_len].chunks_exact_mut(4) {
            pixel.copy_from_slice(&bg);
        }
    }

    let pixel_count = (dst_len / 4).min(src.len() / 4);

    match mode {
        CompositionMode::Overwrite => {
            let byte_count = pixel_count * 4;
            dst[..byte_count].copy_from_slice(&src[..byte_count]);
        },
        CompositionMode::AlphaBlend => {
            for i in 0..pixel_count {
                let off = i * 4;
                alpha_blend_pixel(&mut dst[off..off + 4], &src[off..off + 4]);
            }
        },
    }
}

/// Alpha-blend a single source pixel over a destination pixel (source-over compositing).
///
/// Both slices must be exactly 4 bytes (RGBA).
///
/// Formula (non-premultiplied):
///   out_a = src_a + dst_a × (1 − src_a/255)
///   out_c = (src_c × src_a + dst_c × dst_a × (1 − src_a/255)) / out_a
fn alpha_blend_pixel(dst: &mut [u8], src: &[u8]) {
    let sa = src[3] as u32;
    if sa == 0 {
        return; // Fully transparent source — no change.
    }
    if sa == 255 {
        dst.copy_from_slice(src); // Fully opaque source — replace.
        return;
    }

    let da = dst[3] as u32;
    let inv_sa = 255 - sa;

    // out_a = src_a + dst_a × (1 − src_a/255)
    let out_a = sa + da * inv_sa / 255;

    if out_a == 0 {
        return;
    }

    for c in 0..3 {
        let sc = src[c] as u32;
        let dc = dst[c] as u32;
        // out_c = (src_c × src_a + dst_c × dst_a × (1 − src_a/255)) / out_a
        dst[c] = ((sc * sa + dc * da * inv_sa / 255) / out_a) as u8;
    }
    dst[3] = out_a as u8;
}

/// Unpack a u32 background color to RGBA bytes.
///
/// Byte order matches WezTerm: `0xRRGGBBAA`.
fn background_to_rgba(bg: u32) -> [u8; 4] {
    [
        ((bg >> 24) & 0xff) as u8,
        ((bg >> 16) & 0xff) as u8,
        ((bg >> 8) & 0xff) as u8,
        (bg & 0xff) as u8,
    ]
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Ensure pixel data is RGBA (convert from RGB if needed).
fn ensure_rgba(pixels: &[u8], color_type: ColorType) -> Vec<u8> {
    match color_type {
        ColorType::Rgba => pixels.to_vec(),
        ColorType::Rgb => pixels
            .chunks_exact(3)
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect(),
    }
}

/// Create a background-filled canvas of the given dimensions.
fn make_background_canvas(width: usize, height: usize, background: u32) -> Vec<u8> {
    let total = width * height * 4;
    if background == 0 {
        return vec![0u8; total];
    }
    let bg = background_to_rgba(background);
    let mut canvas = vec![0u8; total];
    for pixel in canvas.chunks_exact_mut(4) {
        pixel.copy_from_slice(&bg);
    }
    canvas
}

/// Place source pixels onto a full-canvas-sized transparent buffer at position (x, y).
fn place_on_canvas(
    canvas_width: usize,
    canvas_height: usize,
    src: &[u8],
    src_width: usize,
    src_height: usize,
    x: usize,
    y: usize,
) -> Vec<u8> {
    let mut canvas = vec![0u8; canvas_width * canvas_height * 4];
    for row in 0..src_height {
        let dest_row = y + row;
        if dest_row >= canvas_height {
            break;
        }
        let copy_cols = src_width.min(canvas_width.saturating_sub(x));
        if copy_cols == 0 {
            continue;
        }
        let src_off = row * src_width * 4;
        let dst_off = (dest_row * canvas_width + x) * 4;
        let byte_count = copy_cols * 4;
        if src_off + byte_count <= src.len() && dst_off + byte_count <= canvas.len() {
            canvas[dst_off..dst_off + byte_count]
                .copy_from_slice(&src[src_off..src_off + byte_count]);
        }
    }
    canvas
}

// ── Action Handlers ────────────────────────────────────────────────────

/// Handle `a=f` (TransmitFrame) — decode payload and add/edit an animation frame.
///
/// Follows the lazy promotion pattern: single-frame images stay as plain `KittyImage`
/// until a second frame arrives, at which point an `AnimationState` is created.
///
/// Parser key overloading (action `f`):
/// - `r=` → frame number to edit (1-based, 0 = append)
/// - `z=` → frame duration in ms
/// - `X=` → composition mode (0 = overwrite, 1 = alpha blend)
/// - `Y=` → background pixel (RGBA packed u32)
/// - `c=` → base frame to copy from (1-based)
/// - `x=`, `y=` → blit position within the canvas
pub fn load_animation_frame(
    state: &mut KittyState,
    cmd: &KittyCommand,
    payload: &[u8],
) -> Result<(), String> {
    // Decode the transmitted pixel data.
    let graphic_data = decode_payload(cmd, payload).map_err(|e| e.to_string())?;

    let image_id = cmd.image_id;
    if image_id == 0 {
        return Err("image_id is required for animation frames".into());
    }

    // Read animation parameters from overloaded parser fields.
    let frame_number = cmd.rows; // r= → frame to edit (1-based, 0 = append)
    let gap_ms = cmd.z_index.max(0) as u32; // z= → frame duration in ms
    let compose_mode = if cmd.offset_x == 1 {
        CompositionMode::AlphaBlend
    } else {
        CompositionMode::Overwrite
    };
    let background = cmd.offset_y; // Y= → background pixel color
    let base_frame = cmd.columns; // c= → base frame to copy from (1-based)
    let dest_x = cmd.src_x as usize; // x= → blit x position
    let dest_y = cmd.src_y as usize; // y= → blit y position

    // Convert decoded data to RGBA.
    let src_pixels = ensure_rgba(&graphic_data.pixels, graphic_data.color_type);
    let src_width = graphic_data.width;
    let src_height = graphic_data.height;

    // Lazy promotion: if no animation exists yet, create one from the existing image.
    if !state.animation_states.contains_key(&image_id) {
        // Use direct field access to avoid borrowing all of `state` through a method.
        let img = state
            .images
            .get(&image_id)
            .ok_or_else(|| format!("image id={image_id} not found"))?;

        let first_pixels = ensure_rgba(&img.data.pixels, img.data.color_type);
        let first_frame = AnimationFrame {
            width: img.data.width,
            height: img.data.height,
            pixels: first_pixels,
            gap_ms: 0,
            composition_mode: CompositionMode::Overwrite,
            background: 0,
        };

        state
            .animation_states
            .insert(image_id, AnimationState::new(first_frame));
        debug!("[kitty] promoted image id={image_id} to animation");
    }

    let anim = state.animation_states.get_mut(&image_id).unwrap();
    let canvas_width = anim.frames[0].width;
    let canvas_height = anim.frames[0].height;

    // Determine whether to append or edit an existing frame (1-based → 0-based).
    let edit_index = if frame_number > 0 && (frame_number as usize) <= anim.frames.len() {
        Some(frame_number as usize - 1)
    } else {
        None // Append new frame.
    };

    // Build the canvas for this frame.
    let mut canvas = if base_frame > 0 {
        // Start from a copy of the base frame.
        let base_idx = (base_frame as usize).saturating_sub(1);
        if base_idx < anim.frames.len() {
            anim.frames[base_idx].pixels.clone()
        } else {
            make_background_canvas(canvas_width, canvas_height, background)
        }
    } else if let Some(idx) = edit_index {
        // Editing an existing frame — start from its current pixels.
        anim.frames[idx].pixels.clone()
    } else {
        // New frame with background fill.
        make_background_canvas(canvas_width, canvas_height, background)
    };

    // Blit the transmitted pixels onto the canvas.
    if dest_x == 0 && dest_y == 0 && src_width == canvas_width && src_height == canvas_height {
        // Fast path: full-frame blit (no positioning needed).
        blit(&mut canvas, &src_pixels, canvas_width, canvas_height, compose_mode, 0);
    } else {
        // Place source pixels on a full-canvas transparent buffer, then blit.
        let placed = place_on_canvas(
            canvas_width,
            canvas_height,
            &src_pixels,
            src_width,
            src_height,
            dest_x,
            dest_y,
        );
        blit(&mut canvas, &placed, canvas_width, canvas_height, compose_mode, 0);
    }

    let new_frame = AnimationFrame {
        pixels: canvas,
        width: canvas_width,
        height: canvas_height,
        gap_ms,
        composition_mode: compose_mode,
        background,
    };

    anim.add_frame(new_frame, edit_index);
    debug!(
        "[kitty] animation id={image_id}: {} (total {} frames)",
        if edit_index.is_some() { "edited frame" } else { "added frame" },
        anim.frames.len()
    );

    Ok(())
}

/// Handle `a=c` (ComposeFrames) — copy pixel data between frames.
///
/// Copies (composites) pixel data from the source frame to the target frame
/// within the same animation.
///
/// Parser key overloading (action `c`):
/// - `c=` → source frame (1-based)
/// - `r=` → target frame (1-based)
/// - `X=` → composition mode (0 = overwrite, 1 = alpha blend)
pub fn compose_frames(state: &mut KittyState, cmd: &KittyCommand) -> Result<(), String> {
    let image_id = cmd.image_id;
    if image_id == 0 {
        return Err("image_id is required for compose".into());
    }

    // c= → source frame (1-based), r= → target frame (1-based).
    let src_frame_no = cmd.columns as usize;
    let target_frame_no = cmd.rows as usize;

    if src_frame_no == 0 {
        return Err("source frame must be > 0".into());
    }
    if target_frame_no == 0 {
        return Err("target frame must be > 0".into());
    }

    let anim = state
        .animation_states
        .get_mut(&image_id)
        .ok_or_else(|| format!("no animation for image id={image_id}"))?;

    let src_idx = src_frame_no - 1;
    let target_idx = target_frame_no - 1;

    if src_idx >= anim.frames.len() {
        return Err(format!(
            "source frame {} out of range (have {} frames)",
            src_frame_no,
            anim.frames.len()
        ));
    }
    if target_idx >= anim.frames.len() {
        return Err(format!(
            "target frame {} out of range (have {} frames)",
            target_frame_no,
            anim.frames.len()
        ));
    }

    let compose_mode = if cmd.offset_x == 1 {
        CompositionMode::AlphaBlend
    } else {
        CompositionMode::Overwrite
    };

    // Clone source pixels to avoid simultaneous mutable + immutable borrow.
    let src_pixels = anim.frames[src_idx].pixels.clone();
    let width = anim.frames[src_idx].width;
    let height = anim.frames[src_idx].height;

    blit(
        &mut anim.frames[target_idx].pixels,
        &src_pixels,
        width,
        height,
        compose_mode,
        0,
    );

    debug!(
        "[kitty] composed frame {} → {} for image id={image_id}",
        src_frame_no, target_frame_no
    );

    Ok(())
}

/// Handle `a=a` (AnimationControl) — control playback state, loops, and current frame.
///
/// Parser key overloading (action `a`):
/// - `s=` (parsed as `cmd.width`) → animation state: 1=stop, 2=running, 3=loading
/// - `v=` (parsed as `cmd.height`) → loop count (0 = infinite)
/// - `r=` (parsed as `cmd.rows`) → set current frame (1-based)
/// - `z=` (parsed as `cmd.z_index`) → set gap_ms for current frame
pub fn control_animation(state: &mut KittyState, cmd: &KittyCommand) {
    let image_id = cmd.image_id;
    if image_id == 0 {
        return;
    }

    let anim = match state.animation_states.get_mut(&image_id) {
        Some(a) => a,
        None => return,
    };

    // s= (parsed as cmd.width) → animation state.
    match cmd.width {
        1 => anim.state = PlaybackState::Stopped,
        2 => anim.state = PlaybackState::Running,
        3 => anim.state = PlaybackState::Loading,
        _ => {},
    }

    // v= (parsed as cmd.height) → loop count (0 = infinite).
    if cmd.height > 0 {
        anim.loops = cmd.height;
    }

    // r= (parsed as cmd.rows) → set current frame (1-based).
    if cmd.rows > 0 {
        let idx = (cmd.rows as usize).saturating_sub(1);
        if idx < anim.frames.len() {
            anim.current_frame = idx;
        }
    }

    // z= (parsed as cmd.z_index) → set gap_ms for current frame.
    if cmd.z_index > 0 {
        let current = anim.current_frame;
        if current < anim.frames.len() {
            anim.frames[current].gap_ms = cmd.z_index as u32;
        }
    }

    debug!(
        "[kitty] animation control id={image_id}: state={:?}, loops={}, current={}",
        anim.state, anim.loops, anim.current_frame
    );
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::kitty_parser::{Action, Format, KittyCommand};
    use crate::graphics::{ColorType, GraphicData, GraphicId};
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;

    /// Helper: create a simple animation frame with overwrite mode.
    fn make_frame(pixels: Vec<u8>, width: usize, height: usize, gap_ms: u32) -> AnimationFrame {
        AnimationFrame {
            pixels,
            width,
            height,
            gap_ms,
            composition_mode: CompositionMode::Overwrite,
            background: 0,
        }
    }

    /// Helper: create a GraphicData for storing in KittyState.
    fn make_graphic(pixels: Vec<u8>, width: usize, height: usize) -> GraphicData {
        GraphicData {
            id: GraphicId(0),
            width,
            height,
            color_type: ColorType::Rgba,
            pixels,
            is_opaque: false,
        }
    }

    // ── 1. Create AnimationState from a single frame ────────────────

    #[test]
    fn create_animation_state() {
        let frame = make_frame(vec![255, 0, 0, 255], 1, 1, 100);
        let anim = AnimationState::new(frame);

        assert_eq!(anim.frames.len(), 1);
        assert_eq!(anim.current_frame, 0);
        assert_eq!(anim.state, PlaybackState::Loading);
        assert_eq!(anim.loops, 0);
        assert_eq!(anim.loops_done, 0);
        assert_eq!(anim.frames[0].pixels, vec![255, 0, 0, 255]);
        assert_eq!(anim.frames[0].gap_ms, 100);
        assert_eq!(anim.frames[0].width, 1);
        assert_eq!(anim.frames[0].height, 1);
    }

    // ── 2. Add frames, verify frame count and data ──────────────────

    #[test]
    fn add_frames_to_animation() {
        let frame1 = make_frame(vec![255, 0, 0, 255], 1, 1, 100);
        let mut anim = AnimationState::new(frame1);

        let frame2 = make_frame(vec![0, 255, 0, 255], 1, 1, 200);
        anim.add_frame(frame2, None);

        let frame3 = make_frame(vec![0, 0, 255, 255], 1, 1, 150);
        anim.add_frame(frame3, None);

        assert_eq!(anim.frames.len(), 3);
        assert_eq!(anim.frames[0].pixels, vec![255, 0, 0, 255]);
        assert_eq!(anim.frames[1].pixels, vec![0, 255, 0, 255]);
        assert_eq!(anim.frames[2].pixels, vec![0, 0, 255, 255]);
        assert_eq!(anim.frames[0].gap_ms, 100);
        assert_eq!(anim.frames[1].gap_ms, 200);
        assert_eq!(anim.frames[2].gap_ms, 150);
    }

    // ── 3. Edit an existing frame by index ──────────────────────────

    #[test]
    fn edit_frame_by_index() {
        let frame1 = make_frame(vec![255, 0, 0, 255], 1, 1, 100);
        let mut anim = AnimationState::new(frame1);

        let frame2 = make_frame(vec![0, 255, 0, 255], 1, 1, 200);
        anim.add_frame(frame2, None);

        // Edit frame 0: replace red with blue.
        let edit = make_frame(vec![0, 0, 255, 255], 1, 1, 300);
        anim.add_frame(edit, Some(0));

        assert_eq!(anim.frames.len(), 2); // Still 2 frames.
        assert_eq!(anim.frames[0].pixels, vec![0, 0, 255, 255]); // Replaced.
        assert_eq!(anim.frames[0].gap_ms, 300);
        assert_eq!(anim.frames[1].pixels, vec![0, 255, 0, 255]); // Unchanged.
    }

    #[test]
    fn edit_frame_out_of_bounds_appends() {
        let frame1 = make_frame(vec![255, 0, 0, 255], 1, 1, 100);
        let mut anim = AnimationState::new(frame1);

        // Index 5 is out of bounds — should append instead.
        let frame_new = make_frame(vec![0, 255, 0, 255], 1, 1, 50);
        anim.add_frame(frame_new, Some(5));

        assert_eq!(anim.frames.len(), 2);
        assert_eq!(anim.frames[1].pixels, vec![0, 255, 0, 255]);
    }

    // ── 4. blit in Overwrite mode ───────────────────────────────────

    #[test]
    fn blit_overwrite_mode() {
        // 2-pixel destination: red, green.
        let mut dst = vec![255, 0, 0, 255, 0, 255, 0, 255];
        // 2-pixel source: blue, white.
        let src = vec![0, 0, 255, 255, 255, 255, 255, 255];

        blit(&mut dst, &src, 2, 1, CompositionMode::Overwrite, 0);

        // Source completely overwrites destination.
        assert_eq!(dst, vec![0, 0, 255, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn blit_overwrite_replaces_even_transparent() {
        let mut dst = vec![255, 0, 0, 255]; // Opaque red.
        let src = vec![0, 0, 0, 0]; // Fully transparent.

        blit(&mut dst, &src, 1, 1, CompositionMode::Overwrite, 0);

        // Overwrite replaces dst with transparent.
        assert_eq!(dst, vec![0, 0, 0, 0]);
    }

    // ── 5. blit in AlphaBlend mode ──────────────────────────────────

    #[test]
    fn blit_alpha_blend_50_percent() {
        // Opaque black destination.
        let mut dst = vec![0, 0, 0, 255];
        // 50% alpha white source (alpha = 128).
        let src = vec![255, 255, 255, 128];

        blit(&mut dst, &src, 1, 1, CompositionMode::AlphaBlend, 0);

        // sa=128, da=255, inv_sa=127
        // out_a = 128 + 255*127/255 = 128 + 127 = 255
        // out_r = (255*128 + 0*255*127/255) / 255 = 32640/255 = 128
        assert_eq!(dst[0], 128); // R
        assert_eq!(dst[1], 128); // G
        assert_eq!(dst[2], 128); // B
        assert_eq!(dst[3], 255); // A
    }

    #[test]
    fn blit_alpha_blend_transparent_source_unchanged() {
        let mut dst = vec![255, 0, 0, 255]; // Opaque red.
        let src = vec![0, 255, 0, 0]; // Fully transparent green.

        blit(&mut dst, &src, 1, 1, CompositionMode::AlphaBlend, 0);

        // Transparent source should not change destination.
        assert_eq!(dst, vec![255, 0, 0, 255]);
    }

    #[test]
    fn blit_alpha_blend_opaque_source_replaces() {
        let mut dst = vec![255, 0, 0, 255]; // Opaque red.
        let src = vec![0, 255, 0, 255]; // Opaque green.

        blit(&mut dst, &src, 1, 1, CompositionMode::AlphaBlend, 0);

        // Opaque source should fully replace destination.
        assert_eq!(dst, vec![0, 255, 0, 255]);
    }

    #[test]
    fn blit_alpha_blend_onto_transparent_dst() {
        let mut dst = vec![0, 0, 0, 0]; // Fully transparent.
        let src = vec![200, 100, 50, 128]; // 50% alpha.

        blit(&mut dst, &src, 1, 1, CompositionMode::AlphaBlend, 0);

        // Source over transparent: out_a = 128 + 0*127/255 = 128
        // out_c = (sc * 128 + 0) / 128 = sc
        assert_eq!(dst[0], 200);
        assert_eq!(dst[1], 100);
        assert_eq!(dst[2], 50);
        assert_eq!(dst[3], 128);
    }

    // ── 6. Background fill before blit ──────────────────────────────

    #[test]
    fn blit_background_fill_then_alpha_blend() {
        let mut dst = vec![0, 0, 0, 0]; // Transparent.
        let src = vec![0, 0, 0, 0]; // Transparent source.
        // Red background with full alpha: R=0xFF, G=0x00, B=0x00, A=0xFF → 0xFF0000FF.
        let background: u32 = 0xFF_00_00_FF;

        blit(&mut dst, &src, 1, 1, CompositionMode::AlphaBlend, background);

        // Background fills first, then transparent source is blended (no change).
        assert_eq!(dst, vec![255, 0, 0, 255]);
    }

    #[test]
    fn blit_background_fill_then_overwrite() {
        let mut dst = vec![0, 0, 0, 0]; // Transparent.
        let src = vec![0, 0, 255, 255]; // Opaque blue.
        // Green background: 0x00FF00FF.
        let background: u32 = 0x00_FF_00_FF;

        blit(&mut dst, &src, 1, 1, CompositionMode::Overwrite, background);

        // Background fills first, then overwrite replaces with blue.
        assert_eq!(dst, vec![0, 0, 255, 255]);
    }

    #[test]
    fn blit_background_fill_visible_with_partial_alpha_src() {
        let mut dst = vec![0, 0, 0, 0];
        // 50% alpha green source.
        let src = vec![0, 255, 0, 128];
        // Opaque red background: 0xFF0000FF.
        let background: u32 = 0xFF_00_00_FF;

        blit(&mut dst, &src, 1, 1, CompositionMode::AlphaBlend, background);

        // dst is filled with red first, then green at 50% is blended over.
        // sa=128, da=255, inv_sa=127
        // out_a = 128 + 255*127/255 = 255
        // R: (0*128 + 255*255*127/255) / 255 = (0 + 32385) / 255 = 127
        // G: (255*128 + 0*255*127/255) / 255 = 32640/255 = 128
        // B: (0*128 + 0*255*127/255) / 255 = 0
        assert_eq!(dst[0], 127); // R (background red bleeds through)
        assert_eq!(dst[1], 128); // G (source green)
        assert_eq!(dst[2], 0); // B
        assert_eq!(dst[3], 255); // A
    }

    // ── 7. control_animation: start, stop, set loops ────────────────

    #[test]
    fn control_animation_start_stop_loading() {
        let mut state = KittyState::default();
        let frame = make_frame(vec![0; 4], 1, 1, 100);
        state.animation_states.insert(1, AnimationState::new(frame));

        // Start (s=2 → Running).
        control_animation(
            &mut state,
            &KittyCommand { image_id: 1, width: 2, ..Default::default() },
        );
        assert_eq!(state.get_animation(1).unwrap().state, PlaybackState::Running);

        // Stop (s=1 → Stopped).
        control_animation(
            &mut state,
            &KittyCommand { image_id: 1, width: 1, ..Default::default() },
        );
        assert_eq!(state.get_animation(1).unwrap().state, PlaybackState::Stopped);

        // Loading (s=3 → Loading).
        control_animation(
            &mut state,
            &KittyCommand { image_id: 1, width: 3, ..Default::default() },
        );
        assert_eq!(state.get_animation(1).unwrap().state, PlaybackState::Loading);
    }

    #[test]
    fn control_animation_set_loops() {
        let mut state = KittyState::default();
        let frame = make_frame(vec![0; 4], 1, 1, 100);
        state.animation_states.insert(1, AnimationState::new(frame));

        // Set loops to 5 (v=5, parsed as cmd.height).
        control_animation(
            &mut state,
            &KittyCommand { image_id: 1, height: 5, ..Default::default() },
        );
        assert_eq!(state.get_animation(1).unwrap().loops, 5);
    }

    #[test]
    fn control_animation_set_current_frame() {
        let mut state = KittyState::default();
        let frame1 = make_frame(vec![0; 4], 1, 1, 100);
        let frame2 = make_frame(vec![0; 4], 1, 1, 100);
        let mut anim = AnimationState::new(frame1);
        anim.add_frame(frame2, None);
        state.animation_states.insert(1, anim);

        // Set current frame to 2 (r=2, parsed as cmd.rows, 1-based).
        control_animation(
            &mut state,
            &KittyCommand { image_id: 1, rows: 2, ..Default::default() },
        );
        assert_eq!(state.get_animation(1).unwrap().current_frame, 1); // 0-based.
    }

    #[test]
    fn control_animation_set_gap_for_current_frame() {
        let mut state = KittyState::default();
        let frame = make_frame(vec![0; 4], 1, 1, 100);
        state.animation_states.insert(1, AnimationState::new(frame));

        // Set gap_ms to 50 for current frame (z=50, parsed as cmd.z_index).
        control_animation(
            &mut state,
            &KittyCommand { image_id: 1, z_index: 50, ..Default::default() },
        );
        assert_eq!(state.get_animation(1).unwrap().frames[0].gap_ms, 50);
    }

    #[test]
    fn control_animation_noop_for_missing_image() {
        let mut state = KittyState::default();
        // Should not panic or error for a non-existent image.
        control_animation(
            &mut state,
            &KittyCommand { image_id: 999, width: 2, ..Default::default() },
        );
    }

    // ── 8. Frame promotion: KittyImage → AnimationState ─────────────

    #[test]
    fn frame_promotion_on_second_frame() {
        let mut state = KittyState::default();

        // Store a 2×1 RGBA image (red + green pixels).
        let original = make_graphic(
            vec![255, 0, 0, 255, 0, 255, 0, 255],
            2,
            1,
        );
        state.store_image(1, original);

        // No animation yet.
        assert!(state.get_animation(1).is_none());

        // Transmit a second frame (2×1: blue + white).
        let frame_pixels: Vec<u8> = vec![0, 0, 255, 255, 255, 255, 255, 255];
        let encoded = BASE64.encode(&frame_pixels);
        let cmd = KittyCommand {
            action: Action::TransmitFrame,
            format: Format::Rgba,
            width: 2,
            height: 1,
            image_id: 1,
            payload: encoded.into_bytes(),
            ..Default::default()
        };

        let result = load_animation_frame(&mut state, &cmd, &cmd.payload);
        assert!(result.is_ok(), "load_animation_frame failed: {result:?}");

        // Verify promotion to animation.
        let anim = state.get_animation(1).expect("animation should exist");
        assert_eq!(anim.frames.len(), 2);

        // Frame 0 = original image data.
        assert_eq!(anim.frames[0].pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(anim.frames[0].width, 2);
        assert_eq!(anim.frames[0].height, 1);

        // Frame 1 = new frame data (blitted in overwrite mode onto transparent canvas).
        assert_eq!(anim.frames[1].pixels, vec![0, 0, 255, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn frame_promotion_preserves_rgb_as_rgba() {
        let mut state = KittyState::default();

        // Store a 1×1 RGB image (no alpha channel).
        let original = GraphicData {
            id: GraphicId(0),
            width: 1,
            height: 1,
            color_type: ColorType::Rgb,
            pixels: vec![128, 64, 32],
            is_opaque: true,
        };
        state.store_image(2, original);

        // Transmit a second frame in RGBA.
        let frame_pixels: Vec<u8> = vec![0, 0, 0, 255];
        let encoded = BASE64.encode(&frame_pixels);
        let cmd = KittyCommand {
            action: Action::TransmitFrame,
            format: Format::Rgba,
            width: 1,
            height: 1,
            image_id: 2,
            payload: encoded.into_bytes(),
            ..Default::default()
        };

        let result = load_animation_frame(&mut state, &cmd, &cmd.payload);
        assert!(result.is_ok());

        let anim = state.get_animation(2).unwrap();
        assert_eq!(anim.frames.len(), 2);
        // Frame 0: RGB promoted to RGBA with alpha=255.
        assert_eq!(anim.frames[0].pixels, vec![128, 64, 32, 255]);
    }

    #[test]
    fn frame_edit_existing_via_frame_number() {
        let mut state = KittyState::default();

        // Store a 1×1 image and promote to animation.
        state.store_image(1, make_graphic(vec![255, 0, 0, 255], 1, 1));

        // Add a second frame to create animation.
        let frame2 = vec![0, 255, 0, 255];
        let encoded2 = BASE64.encode(&frame2);
        let cmd2 = KittyCommand {
            action: Action::TransmitFrame,
            format: Format::Rgba,
            width: 1,
            height: 1,
            image_id: 1,
            payload: encoded2.into_bytes(),
            ..Default::default()
        };
        load_animation_frame(&mut state, &cmd2, &cmd2.payload).unwrap();
        assert_eq!(state.get_animation(1).unwrap().frames.len(), 2);

        // Edit frame 1 (1-based) with blue.
        let edit_pixels = vec![0, 0, 255, 255];
        let encoded_edit = BASE64.encode(&edit_pixels);
        let cmd_edit = KittyCommand {
            action: Action::TransmitFrame,
            format: Format::Rgba,
            width: 1,
            height: 1,
            image_id: 1,
            rows: 1, // r=1 → edit frame index 1 (1-based).
            payload: encoded_edit.into_bytes(),
            ..Default::default()
        };

        load_animation_frame(&mut state, &cmd_edit, &cmd_edit.payload).unwrap();

        let anim = state.get_animation(1).unwrap();
        assert_eq!(anim.frames.len(), 2); // Still 2 frames.
        // Frame 0 was edited: now blue instead of red.
        assert_eq!(anim.frames[0].pixels, vec![0, 0, 255, 255]);
        // Frame 1 unchanged.
        assert_eq!(anim.frames[1].pixels, vec![0, 255, 0, 255]);
    }

    #[test]
    fn load_animation_frame_missing_image() {
        let mut state = KittyState::default();

        let frame_pixels = vec![0, 0, 0, 255];
        let encoded = BASE64.encode(&frame_pixels);
        let cmd = KittyCommand {
            action: Action::TransmitFrame,
            format: Format::Rgba,
            width: 1,
            height: 1,
            image_id: 999, // Non-existent.
            payload: encoded.into_bytes(),
            ..Default::default()
        };

        let result = load_animation_frame(&mut state, &cmd, &cmd.payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    // ── 9. compose_frames: copy pixels between frames ───────────────

    #[test]
    fn compose_frames_copies_pixels() {
        let mut state = KittyState::default();

        // Create animation with 2 frames: red+green, and all-transparent.
        let frame1 = make_frame(vec![255, 0, 0, 255, 0, 255, 0, 255], 2, 1, 100);
        let frame2 = make_frame(vec![0, 0, 0, 0, 0, 0, 0, 0], 2, 1, 100);
        let mut anim = AnimationState::new(frame1);
        anim.add_frame(frame2, None);
        state.animation_states.insert(1, anim);

        // Compose: copy frame 1 → frame 2 (overwrite mode).
        let cmd = KittyCommand {
            image_id: 1,
            columns: 1, // c= source frame (1-based).
            rows: 2,    // r= target frame (1-based).
            ..Default::default()
        };

        let result = compose_frames(&mut state, &cmd);
        assert!(result.is_ok());

        let anim = state.get_animation(1).unwrap();
        // Target frame should now have source frame's pixels.
        assert_eq!(anim.frames[1].pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
        // Source frame unchanged.
        assert_eq!(anim.frames[0].pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn compose_frames_alpha_blend() {
        let mut state = KittyState::default();

        // Frame 1: opaque black.
        let frame1 = make_frame(vec![0, 0, 0, 255], 1, 1, 100);
        // Frame 2: 50% white.
        let frame2 = make_frame(vec![255, 255, 255, 128], 1, 1, 100);
        let mut anim = AnimationState::new(frame1);
        anim.add_frame(frame2, None);
        state.animation_states.insert(1, anim);

        // Compose frame 2 → frame 1 with alpha blend.
        let cmd = KittyCommand {
            image_id: 1,
            columns: 2,   // c= source (50% white).
            rows: 1,       // r= target (opaque black).
            offset_x: 1,  // X=1 → alpha blend.
            ..Default::default()
        };

        compose_frames(&mut state, &cmd).unwrap();

        let anim = state.get_animation(1).unwrap();
        // 50% white over opaque black: expect ~128 gray.
        assert_eq!(anim.frames[0].pixels[0], 128); // R
        assert_eq!(anim.frames[0].pixels[1], 128); // G
        assert_eq!(anim.frames[0].pixels[2], 128); // B
        assert_eq!(anim.frames[0].pixels[3], 255); // A
    }

    #[test]
    fn compose_frames_invalid_source() {
        let mut state = KittyState::default();
        let frame = make_frame(vec![0; 4], 1, 1, 100);
        state.animation_states.insert(1, AnimationState::new(frame));

        let cmd = KittyCommand {
            image_id: 1,
            columns: 5, // Source frame 5 doesn't exist.
            rows: 1,
            ..Default::default()
        };

        let result = compose_frames(&mut state, &cmd);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("out of range"));
    }

    #[test]
    fn compose_frames_no_animation() {
        let mut state = KittyState::default();

        let cmd = KittyCommand {
            image_id: 42,
            columns: 1,
            rows: 1,
            ..Default::default()
        };

        let result = compose_frames(&mut state, &cmd);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no animation"));
    }

    // ── Helpers / edge cases ────────────────────────────────────────

    #[test]
    fn background_to_rgba_unpacks_correctly() {
        let bg = background_to_rgba(0xFF_80_40_C0);
        assert_eq!(bg, [0xFF, 0x80, 0x40, 0xC0]);
    }

    #[test]
    fn ensure_rgba_passthrough() {
        let rgba = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let result = ensure_rgba(&rgba, ColorType::Rgba);
        assert_eq!(result, rgba);
    }

    #[test]
    fn ensure_rgba_from_rgb() {
        let rgb = vec![10, 20, 30, 40, 50, 60];
        let result = ensure_rgba(&rgb, ColorType::Rgb);
        assert_eq!(result, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn place_on_canvas_with_offset() {
        // 3×2 canvas, place a 1×1 pixel at (1, 1).
        let src = vec![255, 0, 0, 255]; // Single red pixel.
        let canvas = place_on_canvas(3, 2, &src, 1, 1, 1, 1);

        // Expected: 3×2 canvas, all transparent except (1,1) which is red.
        assert_eq!(canvas.len(), 3 * 2 * 4); // 24 bytes.
        // Row 0: 3 transparent pixels.
        assert_eq!(&canvas[0..12], &[0; 12]);
        // Row 1: transparent, red, transparent.
        assert_eq!(&canvas[12..16], &[0, 0, 0, 0]); // (0,1)
        assert_eq!(&canvas[16..20], &[255, 0, 0, 255]); // (1,1)
        assert_eq!(&canvas[20..24], &[0, 0, 0, 0]); // (2,1)
    }
}