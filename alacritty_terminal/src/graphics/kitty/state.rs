//! Kitty graphics image storage and state management.

use std::collections::{HashMap, HashSet};



use super::animation::AnimationState;
use crate::graphics::kitty_parser::{DeleteTarget, KittyCommand};
use crate::graphics::GraphicData;

/// Maximum total memory for kitty image storage (320 MiB).
const STORAGE_QUOTA: usize = 320 * 1024 * 1024;

/// A stored kitty image, potentially with multiple placements on screen.
#[derive(Debug)]
pub struct KittyImage {
    /// The decoded pixel data. Retained for re-placement and animation.
    pub data: GraphicData,
    /// Size of the pixel data in bytes (for quota tracking).
    pub total_bytes: usize,
}

/// Accumulator for chunked image transfers.
#[derive(Debug)]
pub struct KittyLoadingImage {
    /// The command from the first chunk (carries format, medium, etc.).
    pub command: KittyCommand,
    /// Accumulated decoded bytes across chunks.
    ///
    /// Each chunk's base64 is decoded on arrival and the raw bytes are
    /// appended here. This handles clients that independently base64-encode
    /// each chunk (e.g. chafa), which would corrupt group alignment if we
    /// concatenated base64 strings and decoded once at the end.
    pub data: Vec<u8>,
}

/// A tracked kitty image placement on the terminal grid.
///
/// Coordinates are 0-based, matching the internal grid coordinate system.
/// The `row` field is `i32` to represent scrollback (negative values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyPlacement {
    /// Image ID this placement refers to.
    pub image_id: u32,
    /// Placement ID (0 = unspecified / default).
    pub placement_id: u32,
    /// Column where the top-left corner is placed (0-based).
    pub col: usize,
    /// Row where the top-left corner is placed (0-based, negative for scrollback).
    pub row: i32,
    /// Number of columns the placement spans.
    pub width: usize,
    /// Number of rows the placement spans.
    pub height: usize,
    /// Z-index for layering.
    pub z_index: i32,
}

impl KittyPlacement {
    /// Check whether this placement overlaps the given column.
    fn overlaps_col(&self, col: usize) -> bool {
        col >= self.col && col < self.col + self.width
    }

    /// Check whether this placement overlaps the given row.
    fn overlaps_row(&self, row: i32) -> bool {
        row >= self.row && row < self.row + self.height as i32
    }

    /// Check whether this placement overlaps the given cell.
    fn overlaps_cell(&self, col: usize, row: i32) -> bool {
        self.overlaps_col(col) && self.overlaps_row(row)
    }
}

/// Kitty graphics protocol state, stored inside `Graphics`.
#[derive(Debug, Default)]
pub struct KittyState {
    /// Stored images keyed by image ID.
    pub images: HashMap<u32, KittyImage>,
    /// Mapping from image number (`I=`) to image ID (`i=`).
    pub number_to_id: HashMap<u32, u32>,
    /// Animation states keyed by image ID.
    pub animation_states: HashMap<u32, AnimationState>,
    /// Image currently being loaded via chunked transfer.
    pub loading: Option<KittyLoadingImage>,
    /// Total bytes used by stored images (for quota enforcement).
    pub used_memory: usize,
    /// Tracked placements on the terminal grid.
    pub placements: Vec<KittyPlacement>,
    /// Next auto-assigned image ID (when client doesn't provide one).
    next_image_id: u32,
}

impl KittyState {
    /// Allocate a new unique image ID.
    pub fn next_id(&mut self) -> u32 {
        self.next_image_id = self.next_image_id.wrapping_add(1);
        if self.next_image_id == 0 {
            self.next_image_id = 1;
        }
        self.next_image_id
    }

    /// Evict unreferenced images until we're under the storage quota.
    pub fn enforce_quota(&mut self) {
        if self.used_memory <= STORAGE_QUOTA {
            return;
        }

        let target_free = self.used_memory - STORAGE_QUOTA;
        let mut freed = 0usize;

        let ids_to_remove: Vec<u32> = self
            .images
            .iter()
            .filter_map(|(&id, img)| {
                if freed < target_free {
                    freed += img.total_bytes;
                    Some(id)
                } else {
                    None
                }
            })
            .collect();

        for id in ids_to_remove {
            if let Some(img) = self.images.remove(&id) {
                self.used_memory = self.used_memory.saturating_sub(img.total_bytes);
            }
        }

        self.number_to_id.retain(|_, img_id| self.images.contains_key(img_id));
    }

    /// Store an image. Enforces quota before inserting.
    pub fn store_image(&mut self, image_id: u32, data: GraphicData) -> u32 {
        let total_bytes = data.pixels.len();

        if let Some(old) = self.images.remove(&image_id) {
            self.used_memory = self.used_memory.saturating_sub(old.total_bytes);
        }

        self.used_memory += total_bytes;
        self.enforce_quota();

        self.images.insert(image_id, KittyImage { data, total_bytes });

        image_id
    }

    /// Look up an image by ID.
    pub fn get_image(&self, image_id: u32) -> Option<&KittyImage> {
        self.images.get(&image_id)
    }

    /// Resolve an image number to an image ID, if mapped.
    pub fn resolve_number(&self, image_number: u32) -> Option<u32> {
        self.number_to_id.get(&image_number).copied()
    }

    /// Register a placement on the grid.
    ///
    /// This should be called from `placement.rs`'s `place_image()` after the
    /// image has been placed on the grid. Coordinates are 0-based.
    pub fn register_placement(&mut self, placement: KittyPlacement) {
        self.placements.push(placement);
    }

    /// Remove placements matching a predicate, returning the set of image IDs
    /// that had at least one placement removed.
    fn remove_placements_matching<F>(&mut self, predicate: F) -> HashSet<u32>
    where
        F: Fn(&KittyPlacement) -> bool,
    {
        let mut removed_image_ids = HashSet::new();
        self.placements.retain(|p| {
            if predicate(p) {
                removed_image_ids.insert(p.image_id);
                false
            } else {
                true
            }
        });
        removed_image_ids
    }

    /// Remove images whose IDs are in `ids` and that are no longer referenced
    /// by any remaining placement. Also cleans up `number_to_id` mappings.
    fn remove_orphaned_images(&mut self, ids: &HashSet<u32>) {
        for &id in ids {
            let still_referenced = self.placements.iter().any(|p| p.image_id == id);
            if !still_referenced {
                if let Some(img) = self.images.remove(&id) {
                    self.used_memory = self.used_memory.saturating_sub(img.total_bytes);
                }
                self.number_to_id.retain(|_, img_id| *img_id != id);
                self.animation_states.remove(&id);
            }
        }
    }

    /// Resolve a 1-based column value from the kitty protocol to 0-based,
    /// falling back to `cursor_col` when the value is 0 (not specified).
    fn resolve_col(src_x: u32, cursor_col: usize) -> usize {
        if src_x > 0 { (src_x - 1) as usize } else { cursor_col }
    }

    /// Resolve a 1-based row value from the kitty protocol to 0-based,
    /// falling back to `cursor_row` when the value is 0 (not specified).
    fn resolve_row(src_y: u32, cursor_row: usize) -> i32 {
        if src_y > 0 { src_y as i32 - 1 } else { cursor_row as i32 }
    }

    /// Look up an animation state by image ID.
    pub fn get_animation(&self, image_id: u32) -> Option<&AnimationState> {
        self.animation_states.get(&image_id)
    }

    /// Look up a mutable animation state by image ID.
    pub fn get_animation_mut(&mut self, image_id: u32) -> Option<&mut AnimationState> {
        self.animation_states.get_mut(&image_id)
    }

    /// Delete images and/or placements according to the given target.
    ///
    /// `cursor_col` and `cursor_row` are 0-based grid coordinates of the
    /// terminal cursor, used for cursor-relative and fallback positioning.
    pub fn delete(
        &mut self,
        target: DeleteTarget,
        cmd: &KittyCommand,
        cursor_col: usize,
        cursor_row: usize,
    ) {
        match target {
            // ── All ────────────────────────────────────────────────────
            DeleteTarget::All | DeleteTarget::AllIncludingScrollback => {
                self.used_memory = 0;
                self.images.clear();
                self.number_to_id.clear();
                self.placements.clear();
                self.animation_states.clear();
            },

            // ── By image ID ───────────────────────────────────────────
            DeleteTarget::ById | DeleteTarget::ByIdIncludingScrollback => {
                if cmd.image_id != 0 {
                    self.placements.retain(|p| p.image_id != cmd.image_id);
                    if let Some(img) = self.images.remove(&cmd.image_id) {
                        self.used_memory = self.used_memory.saturating_sub(img.total_bytes);
                    }
                    self.number_to_id.retain(|_, &mut id| id != cmd.image_id);
                    self.animation_states.remove(&cmd.image_id);
                }
            },

            // ── By image number ───────────────────────────────────────
            DeleteTarget::ByNumber | DeleteTarget::ByNumberIncludingScrollback => {
                if cmd.image_number != 0 {
                    if let Some(image_id) = self.number_to_id.remove(&cmd.image_number) {
                        self.placements.retain(|p| p.image_id != image_id);
                        if let Some(img) = self.images.remove(&image_id) {
                            self.used_memory = self.used_memory.saturating_sub(img.total_bytes);
                        }
                        self.animation_states.remove(&image_id);
                    }
                }
            },

            // ── At cursor position ────────────────────────────────────
            DeleteTarget::AtCursor => {
                let col = cursor_col;
                let row = cursor_row as i32;
                self.remove_placements_matching(|p| p.overlaps_cell(col, row));
            },
            DeleteTarget::AtCursorIncludingScrollback => {
                let col = cursor_col;
                let row = cursor_row as i32;
                let removed = self.remove_placements_matching(|p| p.overlaps_cell(col, row));
                self.remove_orphaned_images(&removed);
            },

            // ── By placement ID ───────────────────────────────────────
            DeleteTarget::ByPlacementId => {
                self.remove_placements_matching(|p| {
                    p.placement_id == cmd.placement_id
                        && (cmd.image_id == 0 || p.image_id == cmd.image_id)
                });
            },
            DeleteTarget::ByPlacementIdIncludingScrollback => {
                let removed = self.remove_placements_matching(|p| {
                    p.placement_id == cmd.placement_id
                        && (cmd.image_id == 0 || p.image_id == cmd.image_id)
                });
                self.remove_orphaned_images(&removed);
            },

            // ── By column ─────────────────────────────────────────────
            DeleteTarget::ByColumn => {
                let col = Self::resolve_col(cmd.src_x, cursor_col);
                self.remove_placements_matching(|p| p.overlaps_col(col));
            },
            DeleteTarget::ByColumnIncludingScrollback => {
                let col = Self::resolve_col(cmd.src_x, cursor_col);
                let removed = self.remove_placements_matching(|p| p.overlaps_col(col));
                self.remove_orphaned_images(&removed);
            },

            // ── By row ────────────────────────────────────────────────
            DeleteTarget::ByRow => {
                let row = Self::resolve_row(cmd.src_y, cursor_row);
                self.remove_placements_matching(|p| p.overlaps_row(row));
            },
            DeleteTarget::ByRowIncludingScrollback => {
                let row = Self::resolve_row(cmd.src_y, cursor_row);
                let removed = self.remove_placements_matching(|p| p.overlaps_row(row));
                self.remove_orphaned_images(&removed);
            },

            // ── By cell position ──────────────────────────────────────
            DeleteTarget::ByCell => {
                let col = Self::resolve_col(cmd.src_x, cursor_col);
                let row = Self::resolve_row(cmd.src_y, cursor_row);
                self.remove_placements_matching(|p| p.overlaps_cell(col, row));
            },
            DeleteTarget::ByCellIncludingScrollback => {
                let col = Self::resolve_col(cmd.src_x, cursor_col);
                let row = Self::resolve_row(cmd.src_y, cursor_row);
                let removed = self.remove_placements_matching(|p| p.overlaps_cell(col, row));
                self.remove_orphaned_images(&removed);
            },

            // ── By cell position + z-index ────────────────────────────
            DeleteTarget::ByCellZ => {
                let col = Self::resolve_col(cmd.src_x, cursor_col);
                let row = Self::resolve_row(cmd.src_y, cursor_row);
                let z = cmd.z_index;
                self.remove_placements_matching(|p| {
                    p.overlaps_cell(col, row) && p.z_index == z
                });
            },
            DeleteTarget::ByCellZIncludingScrollback => {
                let col = Self::resolve_col(cmd.src_x, cursor_col);
                let row = Self::resolve_row(cmd.src_y, cursor_row);
                let z = cmd.z_index;
                let removed = self.remove_placements_matching(|p| {
                    p.overlaps_cell(col, row) && p.z_index == z
                });
                self.remove_orphaned_images(&removed);
            },

            // ── By z-index ────────────────────────────────────────────
            DeleteTarget::ByZIndex => {
                let z = cmd.z_index;
                self.remove_placements_matching(|p| p.z_index == z);
            },
            DeleteTarget::ByZIndexIncludingScrollback => {
                let z = cmd.z_index;
                let removed = self.remove_placements_matching(|p| p.z_index == z);
                self.remove_orphaned_images(&removed);
            },

            // ── Animation frames ──────────────────────────────────────
            DeleteTarget::AnimationFrames
            | DeleteTarget::AnimationFramesIncludingScrollback => {
                if cmd.image_id != 0 {
                    self.animation_states.remove(&cmd.image_id);
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{ColorType, GraphicId};

    fn make_graphic(size: usize) -> GraphicData {
        GraphicData {
            id: GraphicId(0),
            width: 1,
            height: 1,
            color_type: ColorType::Rgba,
            pixels: vec![0; size],
            is_opaque: false,
        }
    }

    fn default_cmd() -> KittyCommand {
        KittyCommand::default()
    }

    /// Build a state with known images and placements for testing.
    ///
    /// Layout (0-based grid coordinates):
    ///
    /// - Image 1, placement 10: col=0, row=0, 3×2, z=0
    /// - Image 1, placement 11: col=5, row=5, 2×2, z=1
    /// - Image 2, placement 20: col=2, row=1, 4×3, z=0
    /// - Image 3, placement 30: col=8, row=0, 1×1, z=-1
    /// - Image 3, placement 31: col=0, row=4, 10×1, z=2
    fn make_test_state() -> KittyState {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(16));
        state.store_image(2, make_graphic(32));
        state.store_image(3, make_graphic(8));

        state.number_to_id.insert(100, 1);
        state.number_to_id.insert(200, 2);

        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 0, width: 3, height: 2, z_index: 0,
        });
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 11, col: 5, row: 5, width: 2, height: 2, z_index: 1,
        });
        state.register_placement(KittyPlacement {
            image_id: 2, placement_id: 20, col: 2, row: 1, width: 4, height: 3, z_index: 0,
        });
        state.register_placement(KittyPlacement {
            image_id: 3, placement_id: 30, col: 8, row: 0, width: 1, height: 1, z_index: -1,
        });
        state.register_placement(KittyPlacement {
            image_id: 3, placement_id: 31, col: 0, row: 4, width: 10, height: 1, z_index: 2,
        });

        assert_eq!(state.placements.len(), 5);
        assert_eq!(state.images.len(), 3);
        state
    }

    // ── Existing tests (updated signatures) ────────────────────────────

    #[test]
    fn store_and_retrieve() {
        let mut state = KittyState::default();
        state.store_image(42, make_graphic(16));
        assert!(state.get_image(42).is_some());
        assert_eq!(state.used_memory, 16);
    }

    #[test]
    fn number_to_id() {
        let mut state = KittyState::default();
        state.number_to_id.insert(100, 42);
        assert_eq!(state.resolve_number(100), Some(42));
        assert_eq!(state.resolve_number(999), None);
    }

    #[test]
    fn delete_by_id() {
        let mut state = KittyState::default();
        state.store_image(10, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 10, placement_id: 1, col: 0, row: 0, width: 1, height: 1, z_index: 0,
        });
        assert!(state.get_image(10).is_some());

        let cmd = KittyCommand { image_id: 10, ..default_cmd() };
        state.delete(DeleteTarget::ById, &cmd, 0, 0);
        assert!(state.get_image(10).is_none());
        assert_eq!(state.used_memory, 0);
        assert!(state.placements.is_empty());
    }

    #[test]
    fn delete_all() {
        let mut state = KittyState::default();
        for id in 1..=5 {
            state.store_image(id, make_graphic(4));
            state.register_placement(KittyPlacement {
                image_id: id, placement_id: id, col: 0, row: 0, width: 1, height: 1, z_index: 0,
            });
        }
        assert_eq!(state.images.len(), 5);
        assert_eq!(state.placements.len(), 5);

        let cmd = default_cmd();
        state.delete(DeleteTarget::All, &cmd, 0, 0);
        assert!(state.images.is_empty());
        assert!(state.placements.is_empty());
        assert_eq!(state.used_memory, 0);
    }

    #[test]
    fn auto_id() {
        let mut state = KittyState::default();
        let id1 = state.next_id();
        let id2 = state.next_id();
        assert_ne!(id1, id2);
        assert_ne!(id1, 0);
        assert_ne!(id2, 0);
    }

    // ── Placement registration ─────────────────────────────────────────

    #[test]
    fn register_placement_records_correctly() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 42, col: 3, row: 5, width: 2, height: 4, z_index: -1,
        });

        assert_eq!(state.placements.len(), 1);
        let p = &state.placements[0];
        assert_eq!(p.image_id, 1);
        assert_eq!(p.placement_id, 42);
        assert_eq!(p.col, 3);
        assert_eq!(p.row, 5);
        assert_eq!(p.width, 2);
        assert_eq!(p.height, 4);
        assert_eq!(p.z_index, -1);
    }

    // ── Overlap helpers ────────────────────────────────────────────────

    #[test]
    fn placement_overlap_col() {
        let p = KittyPlacement {
            image_id: 1, placement_id: 0, col: 2, row: 0, width: 3, height: 1, z_index: 0,
        };
        assert!(!p.overlaps_col(1));
        assert!(p.overlaps_col(2));
        assert!(p.overlaps_col(3));
        assert!(p.overlaps_col(4));
        assert!(!p.overlaps_col(5));
    }

    #[test]
    fn placement_overlap_row() {
        let p = KittyPlacement {
            image_id: 1, placement_id: 0, col: 0, row: 3, width: 1, height: 2, z_index: 0,
        };
        assert!(!p.overlaps_row(2));
        assert!(p.overlaps_row(3));
        assert!(p.overlaps_row(4));
        assert!(!p.overlaps_row(5));
    }

    #[test]
    fn placement_overlap_cell() {
        let p = KittyPlacement {
            image_id: 1, placement_id: 0, col: 1, row: 1, width: 3, height: 2, z_index: 0,
        };
        assert!(p.overlaps_cell(2, 2));
        assert!(!p.overlaps_cell(0, 0));
        assert!(!p.overlaps_cell(4, 1));
        assert!(!p.overlaps_cell(1, 3));
    }

    // ── Delete at cursor ───────────────────────────────────────────────

    #[test]
    fn delete_at_cursor() {
        let mut state = make_test_state();

        // Cursor at (1, 1) overlaps placement 10 (0..3, 0..2) and 20 (2..6, 1..4).
        let cmd = default_cmd();
        state.delete(DeleteTarget::AtCursor, &cmd, 1, 1);

        // Placement 10 covers col 0..3, row 0..2 → (1,1) is inside.
        // Placement 20 covers col 2..6, row 1..4 → (1,1) col=1 < 2, NOT inside.
        // So only placement 10 is removed.
        assert_eq!(state.placements.len(), 4);
        assert!(!state.placements.iter().any(|p| p.placement_id == 10));
        // Images should still be present (non-scrollback variant).
        assert!(state.get_image(1).is_some());
    }

    #[test]
    fn delete_at_cursor_including_scrollback_orphans_image() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(16));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 0, width: 2, height: 2, z_index: 0,
        });

        let cmd = default_cmd();
        state.delete(DeleteTarget::AtCursorIncludingScrollback, &cmd, 0, 0);

        assert!(state.placements.is_empty());
        // Image 1 is now orphaned and should be removed.
        assert!(state.get_image(1).is_none());
        assert_eq!(state.used_memory, 0);
    }

    #[test]
    fn delete_at_cursor_scrollback_keeps_shared_image() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(16));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 0, width: 2, height: 2, z_index: 0,
        });
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 11, col: 5, row: 5, width: 1, height: 1, z_index: 0,
        });

        // Cursor at (0, 0) hits placement 10 but not 11.
        let cmd = default_cmd();
        state.delete(DeleteTarget::AtCursorIncludingScrollback, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 1);
        assert_eq!(state.placements[0].placement_id, 11);
        // Image 1 still referenced by placement 11, so kept.
        assert!(state.get_image(1).is_some());
    }

    // ── Delete by placement ID ─────────────────────────────────────────

    #[test]
    fn delete_by_placement_id() {
        let mut state = make_test_state();

        let cmd = KittyCommand { placement_id: 20, ..default_cmd() };
        state.delete(DeleteTarget::ByPlacementId, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 4);
        assert!(!state.placements.iter().any(|p| p.placement_id == 20));
        // Image 2 still in storage (non-scrollback).
        assert!(state.get_image(2).is_some());
    }

    #[test]
    fn delete_by_placement_id_with_image_id_filter() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.store_image(2, make_graphic(4));
        // Two placements with the same placement_id but different image_ids.
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 99, col: 0, row: 0, width: 1, height: 1, z_index: 0,
        });
        state.register_placement(KittyPlacement {
            image_id: 2, placement_id: 99, col: 1, row: 0, width: 1, height: 1, z_index: 0,
        });

        // Delete placement_id=99 with image_id=1 — should only remove the first.
        let cmd = KittyCommand { placement_id: 99, image_id: 1, ..default_cmd() };
        state.delete(DeleteTarget::ByPlacementId, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 1);
        assert_eq!(state.placements[0].image_id, 2);
    }

    #[test]
    fn delete_by_placement_id_scrollback_removes_orphan() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(8));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 50, col: 0, row: 0, width: 1, height: 1, z_index: 0,
        });

        let cmd = KittyCommand { placement_id: 50, ..default_cmd() };
        state.delete(DeleteTarget::ByPlacementIdIncludingScrollback, &cmd, 0, 0);

        assert!(state.placements.is_empty());
        assert!(state.get_image(1).is_none());
        assert_eq!(state.used_memory, 0);
    }

    // ── Delete by z-index ──────────────────────────────────────────────

    #[test]
    fn delete_by_z_index() {
        let mut state = make_test_state();

        // z=0: placements 10 and 20.
        let cmd = KittyCommand { z_index: 0, ..default_cmd() };
        state.delete(DeleteTarget::ByZIndex, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 3);
        assert!(!state.placements.iter().any(|p| p.z_index == 0));
        // Images still present (non-scrollback).
        assert!(state.get_image(1).is_some());
        assert!(state.get_image(2).is_some());
    }

    #[test]
    fn delete_by_z_index_scrollback() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 0, width: 1, height: 1, z_index: 5,
        });

        let cmd = KittyCommand { z_index: 5, ..default_cmd() };
        state.delete(DeleteTarget::ByZIndexIncludingScrollback, &cmd, 0, 0);

        assert!(state.placements.is_empty());
        assert!(state.get_image(1).is_none());
        assert_eq!(state.used_memory, 0);
    }

    #[test]
    fn delete_by_z_index_no_match() {
        let mut state = make_test_state();
        let before = state.placements.len();

        let cmd = KittyCommand { z_index: 999, ..default_cmd() };
        state.delete(DeleteTarget::ByZIndex, &cmd, 0, 0);

        assert_eq!(state.placements.len(), before);
    }

    // ── Delete by column ───────────────────────────────────────────────

    #[test]
    fn delete_by_column() {
        let mut state = make_test_state();

        // Column 9 (1-based src_x=10) — only placement 31 (col 0..10) overlaps col 9.
        // Placement 30 is at col=8 width=1, so it covers col 8 only.
        let cmd = KittyCommand { src_x: 10, ..default_cmd() };
        state.delete(DeleteTarget::ByColumn, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 4);
        assert!(!state.placements.iter().any(|p| p.placement_id == 31));
    }

    #[test]
    fn delete_by_column_spanning() {
        let mut state = make_test_state();

        // Column 3 (1-based src_x=4) — overlaps:
        //   placement 20 (col 2..6) ✓
        //   placement 31 (col 0..10) ✓
        // Does NOT overlap:
        //   placement 10 (col 0..3) — col 3 is NOT < 0+3=3 ✗
        //   placement 11 (col 5..7) ✗
        //   placement 30 (col 8..9) ✗
        let cmd = KittyCommand { src_x: 4, ..default_cmd() };
        state.delete(DeleteTarget::ByColumn, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 3);
        let remaining_ids: Vec<u32> =
            state.placements.iter().map(|p| p.placement_id).collect();
        assert!(remaining_ids.contains(&10));
        assert!(remaining_ids.contains(&11));
        assert!(remaining_ids.contains(&30));
    }

    #[test]
    fn delete_by_column_scrollback_orphans() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 3, row: 0, width: 2, height: 1, z_index: 0,
        });

        // Column 4 (1-based src_x=5) hits placement at col 3..5.
        let cmd = KittyCommand { src_x: 5, ..default_cmd() };
        state.delete(DeleteTarget::ByColumnIncludingScrollback, &cmd, 0, 0);

        assert!(state.placements.is_empty());
        assert!(state.get_image(1).is_none());
    }

    #[test]
    fn delete_by_column_fallback_to_cursor() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 5, row: 0, width: 2, height: 1, z_index: 0,
        });

        // src_x=0 means "use cursor column". Cursor at col=6.
        let cmd = default_cmd();
        state.delete(DeleteTarget::ByColumn, &cmd, 6, 0);

        assert!(state.placements.is_empty());
    }

    // ── Delete by row ──────────────────────────────────────────────────

    #[test]
    fn delete_by_row() {
        let mut state = make_test_state();

        // Row 0 (1-based src_y=1) — overlaps:
        //   placement 10 (row 0..2) ✓
        //   placement 30 (row 0..1) ✓
        let cmd = KittyCommand { src_y: 1, ..default_cmd() };
        state.delete(DeleteTarget::ByRow, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 3);
        assert!(!state.placements.iter().any(|p| p.placement_id == 10));
        assert!(!state.placements.iter().any(|p| p.placement_id == 30));
    }

    #[test]
    fn delete_by_row_scrollback_orphans() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 2, width: 1, height: 3, z_index: 0,
        });

        // Row 3 (1-based src_y=4) hits placement at row 2..5.
        let cmd = KittyCommand { src_y: 4, ..default_cmd() };
        state.delete(DeleteTarget::ByRowIncludingScrollback, &cmd, 0, 0);

        assert!(state.placements.is_empty());
        assert!(state.get_image(1).is_none());
    }

    #[test]
    fn delete_by_row_fallback_to_cursor() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 3, width: 1, height: 2, z_index: 0,
        });

        // src_y=0 means "use cursor row". Cursor at row=4.
        let cmd = default_cmd();
        state.delete(DeleteTarget::ByRow, &cmd, 0, 4);

        assert!(state.placements.is_empty());
    }

    // ── Delete by cell ─────────────────────────────────────────────────

    #[test]
    fn delete_by_cell() {
        let mut state = make_test_state();

        // Cell (3, 2) in 1-based: src_x=4, src_y=3. Overlaps:
        //   placement 20 (col 2..6, row 1..4) ✓
        let cmd = KittyCommand { src_x: 4, src_y: 3, ..default_cmd() };
        state.delete(DeleteTarget::ByCell, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 4);
        assert!(!state.placements.iter().any(|p| p.placement_id == 20));
    }

    #[test]
    fn delete_by_cell_scrollback_orphans() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 0, width: 2, height: 2, z_index: 0,
        });

        let cmd = KittyCommand { src_x: 1, src_y: 1, ..default_cmd() };
        state.delete(DeleteTarget::ByCellIncludingScrollback, &cmd, 0, 0);

        assert!(state.placements.is_empty());
        assert!(state.get_image(1).is_none());
    }

    #[test]
    fn delete_by_cell_fallback_to_cursor() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 3, row: 4, width: 2, height: 2, z_index: 0,
        });

        // Both src_x=0, src_y=0 → use cursor at (4, 5).
        let cmd = default_cmd();
        state.delete(DeleteTarget::ByCell, &cmd, 4, 5);

        assert!(state.placements.is_empty());
    }

    // ── Delete by cell + z-index ───────────────────────────────────────

    #[test]
    fn delete_by_cell_z() {
        let mut state = make_test_state();

        // Cell (5, 5) with z=1. Placement 11 is at (5, 5) 2×2 z=1 → match.
        // Placement 31 is at (0, 4) 10×1 z=2 → overlaps col 5, row 4 only, row 5 not covered.
        let cmd = KittyCommand { src_x: 6, src_y: 6, z_index: 1, ..default_cmd() };
        state.delete(DeleteTarget::ByCellZ, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 4);
        assert!(!state.placements.iter().any(|p| p.placement_id == 11));
    }

    #[test]
    fn delete_by_cell_z_wrong_z_no_match() {
        let mut state = make_test_state();

        // Cell (5, 5) with z=999 — no placement has z=999 at that cell.
        let cmd = KittyCommand { src_x: 6, src_y: 6, z_index: 999, ..default_cmd() };
        state.delete(DeleteTarget::ByCellZ, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 5);
    }

    #[test]
    fn delete_by_cell_z_scrollback_orphans() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 0, width: 2, height: 2, z_index: 7,
        });

        let cmd = KittyCommand { src_x: 1, src_y: 1, z_index: 7, ..default_cmd() };
        state.delete(DeleteTarget::ByCellZIncludingScrollback, &cmd, 0, 0);

        assert!(state.placements.is_empty());
        assert!(state.get_image(1).is_none());
    }

    // ── Delete by number ───────────────────────────────────────────────

    #[test]
    fn delete_by_number() {
        let mut state = make_test_state();

        let cmd = KittyCommand { image_number: 100, ..default_cmd() };
        state.delete(DeleteTarget::ByNumber, &cmd, 0, 0);

        // Image number 100 → image ID 1. Placements 10 and 11 reference image 1.
        assert!(!state.placements.iter().any(|p| p.image_id == 1));
        assert!(state.get_image(1).is_none());
        // Other images untouched.
        assert!(state.get_image(2).is_some());
        assert!(state.get_image(3).is_some());
    }

    // ── Delete all including scrollback ────────────────────────────────

    #[test]
    fn delete_all_including_scrollback() {
        let mut state = make_test_state();

        let cmd = default_cmd();
        state.delete(DeleteTarget::AllIncludingScrollback, &cmd, 0, 0);

        assert!(state.images.is_empty());
        assert!(state.placements.is_empty());
        assert!(state.number_to_id.is_empty());
        assert_eq!(state.used_memory, 0);
    }

    // ── IncludingScrollback preserves images still referenced ──────────

    #[test]
    fn scrollback_preserves_images_with_remaining_placements() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(16));
        state.store_image(2, make_graphic(8));

        // Two placements for image 1 at different z-indices.
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 0, width: 1, height: 1, z_index: 0,
        });
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 11, col: 2, row: 2, width: 1, height: 1, z_index: 5,
        });
        // One placement for image 2 at z=0.
        state.register_placement(KittyPlacement {
            image_id: 2, placement_id: 20, col: 1, row: 1, width: 1, height: 1, z_index: 0,
        });

        // Delete z=0 with scrollback — removes placements 10 and 20.
        let cmd = KittyCommand { z_index: 0, ..default_cmd() };
        state.delete(DeleteTarget::ByZIndexIncludingScrollback, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 1);
        assert_eq!(state.placements[0].placement_id, 11);

        // Image 1 still referenced by placement 11 → kept.
        assert!(state.get_image(1).is_some());
        // Image 2 orphaned → removed.
        assert!(state.get_image(2).is_none());
        assert_eq!(state.used_memory, 16);
    }

    // ── Animation frames stub ──────────────────────────────────────────

    #[test]
    fn delete_animation_frames_is_noop() {
        let mut state = make_test_state();
        let before = state.placements.len();

        let cmd = default_cmd();
        state.delete(DeleteTarget::AnimationFrames, &cmd, 0, 0);
        assert_eq!(state.placements.len(), before);

        state.delete(DeleteTarget::AnimationFramesIncludingScrollback, &cmd, 0, 0);
        assert_eq!(state.placements.len(), before);
    }

    // ── Edge cases ─────────────────────────────────────────────────────

    #[test]
    fn delete_with_no_placements_is_harmless() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));

        let cmd = default_cmd();
        state.delete(DeleteTarget::AtCursor, &cmd, 5, 5);
        assert!(state.get_image(1).is_some());
        assert_eq!(state.used_memory, 4);
    }

    #[test]
    fn delete_by_id_also_removes_placements() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: 0, width: 1, height: 1, z_index: 0,
        });
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 11, col: 1, row: 1, width: 1, height: 1, z_index: 0,
        });

        let cmd = KittyCommand { image_id: 1, ..default_cmd() };
        state.delete(DeleteTarget::ByIdIncludingScrollback, &cmd, 0, 0);

        assert!(state.placements.is_empty());
        assert!(state.get_image(1).is_none());
    }

    #[test]
    fn delete_by_id_zero_is_noop() {
        let mut state = make_test_state();
        let before_placements = state.placements.len();
        let before_images = state.images.len();

        let cmd = KittyCommand { image_id: 0, ..default_cmd() };
        state.delete(DeleteTarget::ById, &cmd, 0, 0);

        assert_eq!(state.placements.len(), before_placements);
        assert_eq!(state.images.len(), before_images);
    }

    #[test]
    fn negative_row_placement_not_hit_by_positive_cursor() {
        let mut state = KittyState::default();
        state.store_image(1, make_graphic(4));
        // Placement in scrollback (negative row).
        state.register_placement(KittyPlacement {
            image_id: 1, placement_id: 10, col: 0, row: -5, width: 3, height: 2, z_index: 0,
        });

        // Cursor at row 0 — should not overlap a placement at row -5..-3.
        let cmd = default_cmd();
        state.delete(DeleteTarget::AtCursor, &cmd, 0, 0);

        assert_eq!(state.placements.len(), 1);
    }

    #[test]
    fn resolve_col_and_row_helpers() {
        // 1-based → 0-based conversion.
        assert_eq!(KittyState::resolve_col(5, 99), 4);
        assert_eq!(KittyState::resolve_col(1, 99), 0);
        // 0 → cursor fallback.
        assert_eq!(KittyState::resolve_col(0, 7), 7);

        assert_eq!(KittyState::resolve_row(5, 99), 4);
        assert_eq!(KittyState::resolve_row(1, 99), 0);
        assert_eq!(KittyState::resolve_row(0, 7), 7);
    }
}