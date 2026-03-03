# Kitty Graphics Protocol — Alacritty Implementation Plan

## Reference Material

| Source | Location | Notes |
|--------|----------|-------|
| Protocol spec | https://sw.kovidgoyal.net/kitty/graphics-protocol/ | Canonical reference |
| Ghostty impl | `../ghostty/src/terminal/kitty/` | Zig, ~6500 LOC, animation parsed but **unimplemented** |
| WezTerm impl | `../wezterm/term/src/terminalstate/kitty.rs` | Rust, ~4200 LOC, animation **working** |
| Alacritty graphics | `alacritty_terminal/src/graphics/` | Sixel works, rendering pipeline reusable |

---

## Architecture Overview

```text
 ┌────────────────────────────────────────────────────────────────────────┐
 │                        CURRENT (Sixel only)                            │
 │                                                                        │
 │  PTY ──► vte-graphics ──► DCS 'q' ──► sixel::Parser ──► GraphicData    │
 │                                                              │         │
 │                                                              ▼         │
 │                                                      insert_graphic()  │
 │                                                              │         │
 │                                                              ▼         │
 │                                                     Grid cells get     │
 │                                                     GraphicCell refs   │
 │                                                              │         │
 │                                    ┌─────────────────────────┘         │
 │                                    ▼                                   │
 │  display::draw() ──► take_queues() ──► GPU upload ──► shader draw      │
 └────────────────────────────────────────────────────────────────────────┘

 ┌──────────────────────────────────────────────────────────────────────────┐
 │                       TARGET (Sixel + Kitty)                             │
 │                                                                          │
 │                        ┌──── DCS 'q' ──► sixel::Parser ──┐               │
 │  PTY ──► vte-graphics ─┤                                  ├► GraphicData │
 │                        └──── APC 'G' ──► kitty::Parser ──┘     │         │
 │                                  │                              │        │
 │                                  │ (responses)                  ▼        │
 │                                  ▼                       insert_graphic()│
 │                            PTY write-back                       │        │
 │                            ESC _Gi=N;OK                         ▼        │
 │                                                          Grid cells      │
 │                                                                │         │
 │                                    ┌───────────────────────────┘         │
 │                                    ▼                                     │
 │  display::draw() ──► take_queues() ──► GPU upload ──► shader draw        │
 │                          │                                               │
 │                          ├► animation tick ──► re-upload changed frames  │
 │                          └► placement management (z-order, delete)       │
 └──────────────────────────────────────────────────────────────────────────┘
```

The key insight: `GraphicData` is **protocol-agnostic**. Once kitty bytes become
a decoded pixel buffer, the entire GPU pipeline (texture upload, cell attachment,
shader rendering) works identically to sixel. The new work is:

1. Getting APC bytes routed to our parser
2. Parsing the kitty key=value protocol
3. Managing kitty-specific state (image IDs, placements, chunked transfer)
4. Animation (multi-frame images, timed playback, frame composition)

---

## Phase 0 — APC Routing in the VTE Layer

**Problem:** `vte-graphics` 0.15 (the VT parser crate) dispatches DCS sequences
but does **not** dispatch APC sequences. The kitty graphics protocol uses APC
(`ESC _G...ST`). This is the first blocker.

**Approach — patch `vte-graphics`:**

The crate is already a fork of `vte` with graphics extensions. Add APC callbacks
following the same pattern as the existing DCS hooks:

```text
File: vte-graphics (vendored or patched dependency)

  trait Perform {
      // Existing:
      fn dcs_hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char);
      fn dcs_put(&mut self, byte: u8);
      fn dcs_unhook(&mut self);

      // NEW:
+     fn apc_start(&mut self);        // ESC _ received
+     fn apc_put(&mut self, byte: u8); // payload bytes
+     fn apc_end(&mut self);           // ST received (ESC \ or BEL)
  }
```

The VTE state machine already recognizes the `APC_STRING` state; it just doesn't
call anything. Wire it up.

**Files touched:**
- `Cargo.toml` — point `vte` dependency to patched version (git or path)
- `alacritty_terminal/src/term/mod.rs` — implement `apc_start/put/end` in `Handler`

**Validation:**
- `printf '\e_Gtest\e\\'` should hit the new handlers
- No regressions in sixel, keyboard, or existing VT behavior

**Estimated effort:** Small. The VTE state machine already parses APC; we just
need to connect the dispatch.

---

## Phase 1 — Static Image Display (MVP)

Goal: `kitty +kitten icat image.png` works.

### 1a. Kitty Key-Value Parser

**New file:** `alacritty_terminal/src/graphics/kitty.rs`

Parse `ESC _G<key>=<value>[,<key>=<value>...];<base64 payload>ESC \`

```text
struct KittyCommand {
    action: Action,           // a= : t, T, q, p, d, f, a, c
    quiet: u8,                // q= : 0, 1, 2
    image_id: u32,            // i=
    image_number: u32,        // I=
    placement_id: u32,        // p=
    format: Format,           // f= : 24, 32, 100 (PNG)
    transmission: Medium,     // t= : d, f, t, s
    compression: Compression, // o= : z (zlib)
    more_chunks: bool,        // m= : 0 or 1
    width: u32,               // s= (pixel width of transmitted data)
    height: u32,              // v= (pixel height of transmitted data)
    // Display/placement keys:
    src_x: u32,               // x= (source rect)
    src_y: u32,               // y=
    src_w: u32,               // w=
    src_h: u32,               // h=
    offset_x: u32,            // X= (cell pixel offset)
    offset_y: u32,            // Y=
    columns: u32,             // c= (display columns)
    rows: u32,                // r= (display rows)
    z_index: i32,             // z=
    cursor_movement: u8,      // C= : 0 (move) or 1 (don't)
    // Animation keys (Phase 3):
    base_frame: u32,          // c= (overloaded in frame context)
    edit_frame: u32,          // r= (overloaded in frame context)
    compose_mode: u8,         // X= (overloaded: 0=blend, 1=overwrite)
    bg_color: u32,            // Y= (overloaded: RGBA background)
    gap_ms: i32,              // z= (overloaded: frame gap)
    anim_state: u8,           // s= (overloaded: 1=stop, 2=loading, 3=run)
    current_frame: u32,       // c= (overloaded: jump-to frame)
    loops: u32,               // v= (overloaded: loop count)
}
```

The parser is a **streaming state machine** (like sixel):
1. `apc_start` → detect leading `G`, start accumulating
2. `apc_put(byte)` → split at `;` → left side: parse key=value pairs; right side: accumulate base64 payload
3. `apc_end` → finalize command, dispatch

**Reference:** Ghostty's `graphics_command.zig` (clean single-char-key parser),
WezTerm's `apc.rs` (Rust, functional style).

**Dependencies (crate):**
- `base64` — base64 decoding (already in ecosystem, check Cargo.lock)
- `flate2` — zlib decompression for `o=z`
- `png` or `image` — PNG decoding for `f=100`

Prefer `png` crate over `image` for minimal footprint. WezTerm uses `image`
(heavy, but they need it for animation blending). We can start with `png` and
add `image` only if needed for Phase 3.

### 1b. Chunked Transfer State

Images larger than 4096 bytes arrive in multiple APC sequences with `m=1`
(more) / `m=0` (last). Need per-image accumulation:

```text
// In Graphics struct:
pub kitty_loading: Option<KittyLoadingImage>,

struct KittyLoadingImage {
    command: KittyCommand,      // from first chunk
    payload: Vec<u8>,           // accumulated base64 bytes
}
```

On `m=0`: decode full payload → decompress → decode format → `GraphicData`.

Only **one** image can be loading at a time (per the spec). A new transmission
with `m=1` cancels any in-progress load.

### 1c. Payload Decode Pipeline

```text
base64 bytes ──► base64::decode() ──► Option<zlib::decompress()> ──► format dispatch
                                                                          │
                                              ┌───────────────────────────┤
                                              ▼               ▼           ▼
                                           f=24 (RGB)     f=32 (RGBA)  f=100 (PNG)
                                              │               │           │
                                              ▼               ▼           ▼
                                          GraphicData { pixels, width, height, color_type }
```

For PNG: use `png` crate to decode → extract RGBA pixels, width, height.
For raw formats: validate `s * v * bpp == len`.

### 1d. Action Dispatch

```text
match command.action {
    Action::Transmit           => store_image(command, pixels),
    Action::TransmitAndDisplay => { store_image(...); place_image(...); },
    Action::Query              => respond_ok_or_error(...),
    Action::Display            => place_image(command),
    Action::Delete             => delete_images(command),
    // Phase 3:
    Action::TransmitFrame      => load_animation_frame(command, pixels),
    Action::AnimationControl   => control_animation(command),
    Action::ComposeFrames      => compose_frames(command),
}
```

### 1e. Image Storage

```text
// In Graphics struct:
pub kitty_images: HashMap<u32, KittyImage>,

struct KittyImage {
    graphic_id: GraphicId,            // our internal monotonic ID
    data: Option<GraphicData>,        // pixel data (for re-placement or animation)
    placements: Vec<KittyPlacement>,  // active placements on screen
    total_bytes: usize,               // for quota tracking
}
```

**Storage quota:** 320 MiB total, LRU eviction of unreferenced images (matching
kitty/ghostty/wezterm). Track total with an atomic counter.

### 1f. Placement → Grid Cells

`place_image()` adapts the kitty placement parameters to call the existing
`insert_graphic()` or a thin wrapper around it:

- Source rect (`x,y,w,h`) → crop `GraphicData` or pass subsection
- Display size (`c,r` columns/rows) → scale to fit
- Z-index → stored in `GraphicCell` (needs new field or separate overlay list)
- `C=1` → skip cursor advancement after insert

For the MVP, we can **ignore** z-index (everything renders above text, like
sixel) and add z-ordering in a later phase.

### 1g. Response Writing

Kitty protocol requires responses written back to the PTY:

```text
ESC _Gi=<id>;OK ESC \              // success
ESC _Gi=<id>;EINVAL:message ESC \  // error
```

Route through `Event::PtyWrite` (already exists for DSR and other responses).
Respect `q=1` (suppress OK) and `q=2` (suppress all).

### 1h. Delete Support

MVP delete modes (matching what WezTerm actually implements):

| `d=` | MVP? | Notes |
|------|------|-------|
| `a/A` | ✅ | Clear all visible placements |
| `i/I` | ✅ | Delete by image ID |
| `n/N` | ✅ | Delete by image number |
| others | ❌ | Stub with no-op initially |

**Files touched (Phase 1):**
- `alacritty_terminal/src/graphics/kitty.rs` — NEW (~800-1200 LOC)
- `alacritty_terminal/src/graphics/mod.rs` — add `KittyImage` storage, `kitty_loading` field
- `alacritty_terminal/src/term/mod.rs` — wire APC hooks → kitty parser, dispatch
- `alacritty_terminal/Cargo.toml` — add `base64`, `flate2`, `png` dependencies
- `Cargo.toml` (workspace) — add new deps

**Validation:**
```sh
# Basic test (requires kitty's kitten CLI or a compatible tool):
kitty +kitten icat /path/to/image.png

# Or manual escape sequence:
python3 -c "
import base64, sys
data = open('test.png', 'rb').read()
payload = base64.b64encode(data).decode()
# Chunked: 4096 byte chunks
chunks = [payload[i:i+4096] for i in range(0, len(payload), 4096)]
for i, chunk in enumerate(chunks):
    m = 1 if i < len(chunks) - 1 else 0
    if i == 0:
        sys.stdout.write(f'\033_Ga=T,f=100,m={m};{chunk}\033\\\\')
    else:
        sys.stdout.write(f'\033_Gm={m};{chunk}\033\\\\')
sys.stdout.flush()
"
```

**Estimated effort:** Medium-large. This is the bulk of the new code. The parser
alone is ~400-600 lines, the state management another 400-600.

---

## Phase 2 — Correctness & Completeness

Goal: pass the kitty graphics protocol test suite, handle edge cases.

### 2a. Full Delete Support

Implement remaining `d=` modes: by cursor position (`c/C`), by cell
coordinates (`p/P`, `x/X`, `y/Y`), by z-index (`z/Z`), by ID range (`r/R`).

Requires tracking which placements overlap which cells — the existing
`GraphicCell` refs in the grid make position-based deletion O(visible area).

### 2b. Image Number Support (`I=`)

When `I=` is used without `i=`, the terminal auto-assigns an `i` and includes
it in the response. Maintain a `HashMap<u32, u32>` mapping `image_number →
newest_image_id`.

### 2c. Source Rect & Scaling

For `a=p` with `x,y,w,h` (source rect) and `c,r` (display columns/rows):
- Extract sub-rectangle from stored image pixels
- Scale to fit the target cell region (nearest-neighbor for speed, bilinear
  for quality — make configurable or pick one)
- Consider whether to do CPU-side scaling or upload full texture + use
  UV coordinates in the shader (GPU-side is better for large images)

**GPU-side approach (preferred):** Store the full texture. Pass source rect as
UV coordinates to the fragment shader. The existing `graphics.f.glsl` already
samples a texture — just adjust UVs. This avoids CPU pixel copies entirely.

### 2d. Pixel Offsets & Sub-cell Positioning

`X=` and `Y=` specify pixel offsets within the top-left cell. The existing
`GraphicCell.offset_x/y` fields already handle this for sixel. Wire them up
for kitty placements.

### 2e. Cursor Movement Control

`C=0` (default): move cursor to the row after the last row of the image.
`C=1`: do not move cursor.

The existing `insert_graphic()` always moves the cursor (sixel behavior).
Add a parameter to control this.

### 2f. Shared Memory & File Transmission

- `t=f` (file): base64-decode the payload to get a file path, read the file.
  **Security:** validate the path (no symlink attacks, no reading sensitive files).
  Consider whether to support this at all — it's only useful locally.
- `t=t` (temp file): same as file but delete after reading. Only delete if
  path contains `tty-graphics-protocol` and is in a system temp directory.
- `t=s` (shared memory): POSIX `shm_open` / Windows `OpenFileMappingW`.
  Read bytes, then `shm_unlink`. Same security considerations.

For an MVP, **only `t=d` (direct)** needs to work. File and shm can be added
later since most tools (icat, etc.) fall back to direct when others aren't
available.

**Files touched (Phase 2):**
- `alacritty_terminal/src/graphics/kitty.rs` — extend all action handlers
- `alacritty_terminal/src/graphics/mod.rs` — source-rect UV support
- `alacritty/src/renderer/graphics/` — shader UV changes if doing GPU-side crop
- `alacritty/src/renderer/graphics/graphics.f.glsl` — UV rect uniforms

**Validation:**
- Kitty's own test script: `kitty +kitten icat --detect-support`
- Manual tests for each delete mode
- Test chunked transfer with images > 4KB

**Estimated effort:** Medium. Mostly filling in stubs and edge cases.

---

## Phase 3 — Animation & Video

Goal: animated images play in the terminal. Video streaming via tools like
`mpv --vo=kitty` or `timg` works.

### 3a. Multi-Frame Image Storage

Extend `KittyImage` to hold multiple frames:

```text
struct KittyImage {
    graphic_id: GraphicId,
    frames: Vec<KittyFrame>,          // frame 0 = root image
    placements: Vec<KittyPlacement>,
    animation: AnimationState,
    total_bytes: usize,
}

struct KittyFrame {
    pixels: Vec<u8>,         // decoded RGBA pixels
    width: u32,
    height: u32,
    gap_ms: i32,             // time to display (-ve = gapless/skip)
    dirty: bool,             // needs GPU re-upload
}

struct AnimationState {
    mode: AnimMode,          // Stopped, Loading, Running
    current_frame: usize,    // index into frames[]
    loops_remaining: u32,    // 0 = infinite, >0 = count down
    last_advance: Instant,   // when current frame was displayed
}

enum AnimMode { Stopped, Loading, Running }
```

### 3b. Frame Loading (`a=f`)

When `a=f` arrives:

1. Decode payload same as Phase 1 (base64 → decompress → format decode)
2. If `r=` (edit frame): modify existing frame's pixels in-place
3. Else: create new frame, composite onto `c=` base frame (or transparent)
4. Set `gap_ms` from `z=` parameter
5. Mark frame as dirty for GPU upload

**Composition** (needed for delta frames):
- `X=0` (default): alpha blend source onto destination
- `X=1`: overwrite destination with source

Use the `image` crate's `imageops::overlay()` for alpha blending.
This is where we upgrade from the `png` crate to `image`.

**WezTerm reference:** `blit()` function in `kitty.rs` handles this with a
`clip_view()` helper for the borrow checker. Their `Rgba8 → AnimRgba8`
promotion pattern (upgrade a single-frame image to multi-frame on second
frame arrival) is clean and worth following.

### 3c. Animation Control (`a=a`)

```text
match command.anim_state {
    1 => image.animation.mode = Stopped,
    2 => image.animation.mode = Loading,   // play to end, wait for more
    3 => image.animation.mode = Running,   // loop normally
    _ => {}
}
if command.current_frame > 0 {
    image.animation.current_frame = command.current_frame - 1;  // 1-based → 0-based
}
if command.loops > 0 {
    image.animation.loops_remaining = command.loops;
}
// Per-frame gap update:
if command.edit_frame > 0 {
    image.frames[command.edit_frame - 1].gap_ms = command.gap_ms;
}
```

### 3d. Frame Composition (`a=c`)

Blit a rectangle from frame `r` onto frame `c` without new pixel data.
Pure CPU-side operation on the stored frame buffers.

### 3e. Animation Tick in the Render Loop

The display draw loop needs to **advance animations** and **re-upload dirty frames**:

```text
// In display::draw(), after take_queues():

let now = Instant::now();
for image in graphics.kitty_images.values_mut() {
    if image.animation.mode == Running || image.animation.mode == Loading {
        let frame = &image.frames[image.animation.current_frame];
        let gap = if frame.gap_ms > 0 { frame.gap_ms as u64 } else { 40 };

        if now.duration_since(image.animation.last_advance) >= Duration::from_millis(gap) {
            // Advance to next frame
            let next = image.animation.current_frame + 1;
            if next >= image.frames.len() {
                if image.animation.mode == Loading {
                    // Stay on last frame, wait for more data
                } else if image.animation.loops_remaining == 0 {
                    image.animation.current_frame = 0; // infinite loop
                } else {
                    image.animation.loops_remaining -= 1;
                    if image.animation.loops_remaining > 0 {
                        image.animation.current_frame = 0;
                    } else {
                        image.animation.mode = Stopped;
                    }
                }
            } else {
                // Skip gapless frames (gap < 0)
                image.animation.current_frame = next;
                while image.animation.current_frame < image.frames.len()
                    && image.frames[image.animation.current_frame].gap_ms < 0
                {
                    image.animation.current_frame += 1;
                }
            }
            image.animation.last_advance = now;

            // Mark the GPU texture as needing re-upload with new frame's pixels
            enqueue_texture_update(image);
        }
    }
}
```

**Critical detail:** Animation requires **re-rendering even when no PTY input arrives**.
The event loop needs a wake-up timer when animations are active. The existing
`display::draw()` is driven by PTY events and window events — add a periodic
tick (e.g., 16ms / 60fps) when at least one animation is running.

**WezTerm reference:** They handle this in `glyphcache.rs` by checking frame
duration on each draw call and clamping to `min_frame_duration`.

### 3f. Efficient Texture Updates

For animation, uploading the entire image every frame is wasteful. Options:

1. **Full re-upload** (simplest): `glTexImage2D` with new pixel data each frame.
   Fine for small images. WezTerm does this.
2. **Sub-rect update**: `glTexSubImage2D` for delta frames that only change a
   region. More efficient for large images with small changes.
3. **Double-buffer**: maintain two GPU textures, swap on frame advance.

Start with option 1 (full re-upload). Profile later.

**Files touched (Phase 3):**
- `alacritty_terminal/src/graphics/kitty.rs` — frame storage, composition, control
- `alacritty_terminal/src/graphics/mod.rs` — animation state, timer integration
- `alacritty/src/display/mod.rs` — animation tick in draw loop
- `alacritty/src/event_loop.rs` or `alacritty/src/event.rs` — animation timer
- `alacritty_terminal/Cargo.toml` — add `image` crate (for overlay/blend ops)

**Validation:**
```sh
# Simple animated GIF via timg:
timg --kitty animated.gif

# mpv video output:
mpv --vo=kitty --vo-kitty-use-shm=no video.mp4

# Manual animation test:
python3 tests/kitty_animation_test.py  # (we'll write this)
```

**Estimated effort:** Large. Frame composition and the render-loop timer are
the trickiest parts.

---

## Phase 4 — Advanced Features (Stretch)

### 4a. Unicode Placeholders (`U+10EEEE`)

Virtual placements allow images inside tmux, vim, and other host programs.
The image is placed by printing a special Unicode character with color attributes
encoding the image/placement ID and diacritics encoding row/column.

This requires:
- Detecting `U+10EEEE` during cell rendering
- Reading fg/underline color to extract image_id/placement_id
- Reading diacritics to determine row/col within the image
- Rendering the appropriate sub-tile of the image texture

Lower priority — most direct use cases (icat, timg, mpv) don't need this.
It's primarily for tmux passthrough and embedding in TUI apps.

### 4b. Relative Placements

`P=<parent_id>,Q=<parent_placement_id>` with `H,V` offsets. Placement
lifetime is tied to parent. Max depth 8. Cycle detection required.

Low priority — used mainly for annotations/overlays on images.

### 4c. Z-Index Rendering

The protocol supports `z=` for layering:
- `z < INT32_MIN/2` → below non-default cell backgrounds
- `z < 0` → below text
- `z >= 0` → above text (default)

Requires sorting placements by z during rendering and potentially multiple
draw passes. Ghostty does 3 passes (below-bg, below-text, above-text).

### 4d. Shared Memory & File Transmission

Lower priority but needed for performance with large images. `t=s` (POSIX
shared memory) avoids base64 overhead entirely. Important for video streaming
over local connections.

---

## Dependency Summary

| Crate | Phase | Purpose | Weight |
|-------|-------|---------|--------|
| `base64` | 1 | Decode APC payloads | Tiny |
| `flate2` | 1 | Zlib decompression (`o=z`) | Small |
| `png` | 1 | PNG decoding (`f=100`) | Small |
| `image` | 3 | Frame composition (overlay/blend) | Medium |

Check `Cargo.lock` — some of these may already be transitive dependencies.
Prefer the lightest option at each phase.

---

## Task Breakdown

### Phase 0: APC Routing
- [ ] P0.1 — Fork/patch `vte-graphics` to add `apc_start/put/end` dispatch
- [ ] P0.2 — Implement no-op `apc_*` methods in `term/mod.rs` Handler
- [ ] P0.3 — Verify APC sequences reach the handler (printf test)

### Phase 1: Static Images (MVP)
- [ ] P1.1 — Create `graphics/kitty.rs` with key-value parser
- [ ] P1.2 — Implement `KittyCommand` struct and streaming parser
- [ ] P1.3 — Implement chunked transfer accumulation (`m=0/1`)
- [ ] P1.4 — Implement payload decode (base64 → zlib → format → GraphicData)
- [ ] P1.5 — Implement `a=t` (transmit/store) with `KittyImage` storage
- [ ] P1.6 — Implement `a=T` (transmit + display) via `insert_graphic()`
- [ ] P1.7 — Implement `a=q` (query) with response writing
- [ ] P1.8 — Implement `a=p` (display previously stored image)
- [ ] P1.9 — Implement `a=d` (delete) — `d=a`, `d=i`, `d=n` modes
- [ ] P1.10 — Add `kitty_graphics` feature flag in config
- [ ] P1.11 — End-to-end test: `kitty +kitten icat` displays an image

### Phase 2: Correctness
- [ ] P2.1 — Full delete mode support (`d=c/p/x/y/z/r` and uppercase)
- [ ] P2.2 — Image number (`I=`) support with auto-ID assignment
- [ ] P2.3 — Source rect display (UV coordinates in shader or CPU crop)
- [ ] P2.4 — Display scaling (`c=`, `r=` columns/rows)
- [ ] P2.5 — Pixel offset (`X=`, `Y=`) integration
- [ ] P2.6 — Cursor movement control (`C=0/1`)
- [ ] P2.7 — Storage quota enforcement (320 MiB cap, LRU eviction)
- [ ] P2.8 — File transmission (`t=f`, `t=t`) with security checks
- [ ] P2.9 — Shared memory transmission (`t=s`)

### Phase 3: Animation & Video
- [ ] P3.1 — Multi-frame `KittyImage` with `Vec<KittyFrame>`
- [ ] P3.2 — Frame loading (`a=f`) with full & delta frames
- [ ] P3.3 — Frame composition (alpha blend + overwrite via `image` crate)
- [ ] P3.4 — Animation control (`a=a`) state machine
- [ ] P3.5 — Frame composition command (`a=c`)
- [ ] P3.6 — Animation tick in display draw loop
- [ ] P3.7 — Event loop timer for animation-driven redraws
- [ ] P3.8 — Efficient texture re-upload for frame changes
- [ ] P3.9 — End-to-end test: animated GIF via timg, video via mpv

### Phase 4: Advanced (Stretch)
- [ ] P4.1 — Unicode placeholder support (`U+10EEEE`)
- [ ] P4.2 — Relative placements (`P=`, `Q=`, `H`, `V`)
- [ ] P4.3 — Z-index multi-pass rendering
- [ ] P4.4 — Shared memory transmission optimization

---

## Testing Strategy

Each phase gets **end-to-end tests** that exercise the full pipeline from escape
sequence input to visible rendering, saving the developer from manual verification.

```text
tests/
├── kitty_graphics/
│   ├── test_parse.rs          # Unit tests for key-value parser
│   ├── test_chunked.rs        # Chunked transfer reassembly
│   ├── test_decode.rs         # base64 → zlib → PNG → pixels
│   ├── test_storage.rs        # Image store, retrieve, quota, eviction
│   ├── test_placement.rs      # Placement creation, source rect, scaling
│   ├── test_delete.rs         # All delete modes
│   ├── test_animation.rs      # Frame loading, composition, playback
│   └── test_response.rs       # Response generation, quiet modes
├── fixtures/
│   ├── 1x1_red.png            # Minimal test image
│   ├── 2x2_rgba.raw           # Raw RGBA pixels
│   └── animated_2frame.bin    # Pre-encoded animation sequence
└── integration/
    ├── test_icat.sh           # Requires kitty CLI tools
    └── test_timg.sh           # Requires timg
```

Tests must be **isolated** — no killing existing terminal processes, no writing
to real PTY devices. Use the `alacritty_terminal::Term` struct directly with a
mock event listener, feeding raw bytes and inspecting grid state.

---

## Risk Register

| Risk | Impact | Mitigation |
|------|--------|------------|
| `vte-graphics` APC patch is complex | Blocks everything | The VTE state machine already parses APC; dispatch is ~20 lines. Low risk. |
| Upstream Alacritty rejects kitty graphics | Fork diverges | Keep changes modular behind feature flag. Minimize changes to core code. |
| Animation timer causes idle CPU burn | Battery drain | Only tick when animations are active. Use `request_redraw()` with delay, not busy-spin. |
| Memory pressure from large images | OOM | Enforce 320 MiB quota from day one. Evict LRU. |
| `image` crate bloats binary | Larger download | Only enable needed decoders (no JPEG, TIFF, etc. — just RGBA operations). |
| Z-index rendering requires shader changes | Rendering regressions | Defer z-index to Phase 4. Default everything to above-text. |
| Security of file/shm transmission | Path traversal, info leak | Phase 2 item. Validate paths strictly. Consider disabling by default. |
