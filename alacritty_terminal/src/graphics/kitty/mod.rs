//! Kitty graphics protocol implementation.
//!
//! This module is split into submodules for parallel development:
//! - `state`: Image storage, quota management, chunked transfer state
//! - `decode`: Payload decode pipeline (base64 → zlib → format → GraphicData)
//! - `placement`: Image placement on the terminal grid, cropping, ID resolution
//! - `response`: Protocol response formatting and PTY writing
//!
//! See: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

pub mod animation;
pub mod decode;
pub mod placement;
pub mod response;
pub mod state;

use log::{debug, trace};

use crate::event::EventListener;
use crate::graphics::kitty_parser::{Action, DeleteTarget, KittyCommand, Medium};
use crate::term::Term;

pub use self::state::{KittyImage, KittyLoadingImage, KittyPlacement, KittyState};

use self::decode::decode_payload;
use self::placement::{place_image, resolve_image_id, resolve_or_assign_id};
use self::response::{error_response, ok_response, send_response};

/// Process a fully parsed kitty graphics command.
///
/// This is the main entry point called from `apc_end` in `term/mod.rs`.
pub fn dispatch_command<L: EventListener>(term: &mut Term<L>, cmd: KittyCommand) {
    // Handle chunked transfer accumulation.
    if cmd.more_chunks {
        handle_chunk_start(term, cmd);
        return;
    }

    // Check if this is the final chunk of a multi-chunk transfer.
    let cmd = if let Some(loading) = term.graphics.kitty_state.loading.take() {
        finalize_chunked(loading, cmd)
    } else {
        cmd
    };

    let quiet = cmd.quiet;
    let image_id = cmd.image_id;

    match cmd.action {
        Action::Transmit => {
            let result = handle_transmit(term, &cmd);
            send_response(term.event_proxy(), quiet, image_id, &result);
        },
        Action::TransmitAndDisplay => {
            let result = handle_transmit_and_display(term, &cmd);
            let resolved_id =
                if image_id != 0 { image_id } else { cmd.image_number };
            send_response(term.event_proxy(), quiet, resolved_id, &result);
        },
        Action::Query => {
            handle_query(term, &cmd);
        },
        Action::Display => {
            let result = handle_display(term, &cmd);
            send_response(term.event_proxy(), quiet, image_id, &result);
        },
        Action::Delete => {
            handle_delete(term, &cmd);
        },
        Action::TransmitFrame => {
            let result = animation::load_animation_frame(
                &mut term.graphics.kitty_state,
                &cmd,
                &cmd.payload,
            );
            send_response(term.event_proxy(), quiet, image_id, &result);
        },
        Action::AnimationControl => {
            animation::control_animation(&mut term.graphics.kitty_state, &cmd);
        },
        Action::ComposeFrames => {
            let result = animation::compose_frames(&mut term.graphics.kitty_state, &cmd);
            send_response(term.event_proxy(), quiet, image_id, &result);
        },
    }
}

// ── Chunked Transfer ───────────────────────────────────────────────────

/// Decode a single chunk's base64 payload into raw bytes.
///
/// For `Medium::Direct`, this decodes base64 immediately. For file/shm
/// mediums the payload is a path, so we pass it through unchanged (it
/// will only appear in single-shot transfers, not chunked).
fn decode_chunk_payload(medium: Medium, payload: &[u8]) -> Vec<u8> {
    if payload.is_empty() {
        return Vec::new();
    }

    if medium != Medium::Direct {
        // File/shm mediums: payload is a base64 path — keep as-is.
        return payload.to_vec();
    }

    // Decode this chunk's base64 independently. This handles clients
    // like chafa that base64-encode each chunk separately (with padding)
    // rather than splitting one big base64 string across chunks.
    use base64::Engine;
    use base64::alphabet;
    use base64::engine::{GeneralPurpose, GeneralPurposeConfig, DecodePaddingMode};

    const B64: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );

    match B64.decode(payload) {
        Ok(bytes) => bytes,
        Err(e) => {
            debug!("[kitty] chunk base64 decode error: {e}");
            Vec::new()
        },
    }
}

/// Start or continue a chunked transfer (m=1).
fn handle_chunk_start<L: EventListener>(term: &mut Term<L>, cmd: KittyCommand) {
    let decoded = decode_chunk_payload(cmd.medium, &cmd.payload);

    match &mut term.graphics.kitty_state.loading {
        Some(loading) => {
            loading.data.extend_from_slice(&decoded);
            trace!(
                "[kitty] chunk appended, total decoded: {} bytes",
                loading.data.len()
            );
        },
        None => {
            trace!("[kitty] starting chunked transfer, first chunk: {} decoded bytes", decoded.len());
            term.graphics.kitty_state.loading = Some(KittyLoadingImage {
                command: cmd,
                data: decoded,
            });
        },
    }
}

/// Finalize a chunked transfer by merging the last chunk.
///
/// Returns a command whose `payload` contains the fully decoded raw bytes
/// and whose `pre_decoded` flag is set so `decode_payload` skips base64.
fn finalize_chunked(mut loading: KittyLoadingImage, final_cmd: KittyCommand) -> KittyCommand {
    let decoded = decode_chunk_payload(loading.command.medium, &final_cmd.payload);
    loading.data.extend_from_slice(&decoded);

    let mut cmd = loading.command;
    cmd.payload = loading.data;
    cmd.more_chunks = false;
    cmd.pre_decoded = true;
    cmd
}

// ── Action Handlers ────────────────────────────────────────────────────

/// Handle `a=t` (transmit only, don't display).
fn handle_transmit<L: EventListener>(
    term: &mut Term<L>,
    cmd: &KittyCommand,
) -> Result<(), String> {
    let graphic_data = decode_payload(cmd, &cmd.payload).map_err(|e| e.to_string())?;
    let image_id = resolve_or_assign_id(term, cmd);
    term.graphics.kitty_state.store_image(image_id, graphic_data);
    debug!("[kitty] stored image id={image_id}");
    Ok(())
}

/// Handle `a=T` (transmit and display).
fn handle_transmit_and_display<L: EventListener>(
    term: &mut Term<L>,
    cmd: &KittyCommand,
) -> Result<(), String> {
    let graphic_data = decode_payload(cmd, &cmd.payload).map_err(|e| e.to_string())?;
    let image_id = resolve_or_assign_id(term, cmd);

    term.graphics.kitty_state.store_image(image_id, graphic_data);
    debug!("[kitty] stored image id={image_id}, placing on grid");

    place_image(term, image_id, cmd)
}

/// Handle `a=q` (query).
fn handle_query<L: EventListener>(term: &mut Term<L>, cmd: &KittyCommand) {
    use crate::event::Event;

    let image_id = if cmd.image_id != 0 { cmd.image_id } else { 1 };

    let result = if cmd.payload.is_empty() {
        Ok(())
    } else {
        decode_payload(cmd, &cmd.payload).map(|_| ())
    };

    match result {
        Ok(()) => {
            let resp = ok_response(image_id);
            trace!("[kitty] query response: {resp:?}");
            term.event_proxy().send_event(Event::PtyWrite(resp));
        },
        Err(e) => {
            let resp = error_response(image_id, "EINVAL", &e.to_string());
            trace!("[kitty] query error response: {resp:?}");
            term.event_proxy().send_event(Event::PtyWrite(resp));
        },
    }
}

/// Handle `a=p` (display a previously transmitted image).
fn handle_display<L: EventListener>(
    term: &mut Term<L>,
    cmd: &KittyCommand,
) -> Result<(), String> {
    let image_id = resolve_image_id(&term.graphics.kitty_state, cmd)
        .ok_or_else(|| "image not found".to_string())?;

    if !term.graphics.kitty_state.images.contains_key(&image_id) {
        return Err(format!("image id={image_id} not found in storage"));
    }

    place_image(term, image_id, cmd)
}

/// Handle `a=d` (delete).
fn handle_delete<L: EventListener>(term: &mut Term<L>, cmd: &KittyCommand) {
    let target = cmd.delete.unwrap_or(DeleteTarget::All);
    let cursor_col = term.grid().cursor.point.column.0;
    let cursor_row = term.grid().cursor.point.line.0 as usize;
    debug!("[kitty] delete: {target:?}, image_id={}, image_number={}", cmd.image_id, cmd.image_number);
    term.graphics.kitty_state.delete(target, cmd, cursor_col, cursor_row);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::kitty_parser::{Action, Format};

    #[test]
    fn finalize_chunked_merges_decoded_bytes() {
        // KittyLoadingImage now holds already-decoded bytes (not base64).
        let loading = KittyLoadingImage {
            command: KittyCommand {
                action: Action::TransmitAndDisplay,
                format: Format::Png,
                image_id: 5,
                ..Default::default()
            },
            data: vec![0xDE, 0xAD],
        };

        // The final chunk's payload is still base64 — finalize_chunked
        // decodes it via decode_chunk_payload before appending.
        use base64::Engine;
        let final_bytes = vec![0xBE, 0xEF];
        let final_b64 = base64::engine::general_purpose::STANDARD.encode(&final_bytes);
        let final_cmd = KittyCommand {
            payload: final_b64.into_bytes(),
            ..Default::default()
        };

        let merged = finalize_chunked(loading, final_cmd);
        assert_eq!(merged.action, Action::TransmitAndDisplay);
        assert_eq!(merged.format, Format::Png);
        assert_eq!(merged.image_id, 5);
        assert_eq!(merged.payload, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(!merged.more_chunks);
        assert!(merged.pre_decoded);
    }
}