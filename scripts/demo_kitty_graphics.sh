#!/usr/bin/env bash
set -euo pipefail

# ── Kitty Graphics Protocol — Real-World Demo ──────────────────────────
#
# Uses timg, chafa, and mpv to exercise the kitty graphics protocol
# against our alacritty-kitty fork with real-world applications.
#
# Usage:
#     # Build and launch the fork first:
#     cd ~/src/wt/kitty/alacritty
#     cargo build && ./target/debug/alacritty
#
#     # Then inside that terminal:
#     bash scripts/demo_kitty_graphics.sh          # run all demos
#     bash scripts/demo_kitty_graphics.sh static   # just static images
#     bash scripts/demo_kitty_graphics.sh anim     # just animated GIF
#     bash scripts/demo_kitty_graphics.sh video    # just video playback
#     bash scripts/demo_kitty_graphics.sh scaling  # scaling comparisons
#     bash scripts/demo_kitty_graphics.sh tools    # tool comparison grid
#     bash scripts/demo_kitty_graphics.sh stress   # large/many images
#
# Prerequisites (brew install timg mpv chafa imagemagick):
#     timg   — terminal image/video viewer with kitty support
#     chafa  — terminal graphics with kitty support
#     mpv    — media player with --vo=kitty
#     magick — ImageMagick for generating test assets
# ────────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ASSET_DIR="$(mktemp -d)"

# Reset terminal modes that tools may enable and fail to clean up on exit.
# Covers: mouse tracking (1000,1002,1003,1006,1015), bracketed paste (2004),
# alternate screen (1049), application cursor keys (1).
reset_term() {
    printf '\033[?1000l\033[?1002l\033[?1003l\033[?1006l\033[?1015l'
    printf '\033[?2004l'
    printf '\033[?1049l'
    printf '\033[?1l'
}

trap 'reset_term; rm -rf "$ASSET_DIR"' EXIT

# ── Colors / helpers ───────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

banner() {
    local width=62
    echo
    printf "${CYAN}╔"
    printf '═%.0s' $(seq 1 $width)
    printf "╗${RESET}\n"
    printf "${CYAN}║${BOLD} %-${width}s${CYAN}║${RESET}\n" "$1"
    if [[ -n "${2:-}" ]]; then
        printf "${CYAN}║${DIM} %-${width}s${CYAN}║${RESET}\n" "$2"
    fi
    printf "${CYAN}╚"
    printf '═%.0s' $(seq 1 $width)
    printf "╝${RESET}\n"
    echo
}

section() {
    echo
    printf "${YELLOW}── %s ──${RESET}\n" "$1"
    echo
}

ok()   { printf "  ${GREEN}✓${RESET} %s\n" "$1"; }
warn() { printf "  ${YELLOW}⚠${RESET} %s\n" "$1"; }
fail() { printf "  ${RED}✗${RESET} %s\n" "$1"; }
info() { printf "  ${DIM}%s${RESET}\n" "$1"; }

pause() {
    echo
    printf "${DIM}  Press Enter to continue...${RESET}"
    read -r
    echo
}

check_tool() {
    if command -v "$1" &>/dev/null; then
        ok "$1 found: $(command -v "$1")"
        return 0
    else
        fail "$1 not found — install with: brew install $1"
        return 1
    fi
}

# ── Asset Generation ───────────────────────────────────────────────────
# Generate test images with ImageMagick so we don't depend on having
# pictures lying around. Every asset is deterministic and disposable.

generate_assets() {
    section "Generating test assets in $ASSET_DIR"

    # 1. Gradient PNG (256x256) — good for scaling tests.
    magick -size 256x256 gradient:red-blue "$ASSET_DIR/gradient.png"
    ok "gradient.png (256×256 red→blue)"

    # 2. Photo-like: plasma fractal (512x384) — exercises PNG decode well.
    magick -size 512x384 plasma:fractal "$ASSET_DIR/plasma.png"
    ok "plasma.png (512×384 fractal)"

    # 3. Tiny icon (16x16) — tests small image handling.
    magick -size 16x16 xc:none \
        -fill '#FF6600' -draw "circle 7,7 7,1" \
        "$ASSET_DIR/icon16.png"
    ok "icon16.png (16×16 circle)"

    # 4. Wide banner (800x100) — tests horizontal scroll / clipping.
    magick -size 800x100 gradient:cyan-magenta "$ASSET_DIR/banner.png"
    ok "banner.png (800×100 wide)"

    # 5. Tall strip (100x600) — tests vertical clipping.
    magick -size 100x600 gradient:yellow-green "$ASSET_DIR/tall.png"
    ok "tall.png (100×600 tall)"

    # 6. Animated GIF — 8 frames, color cycling, 150ms per frame.
    local frames=()
    for i in $(seq 0 7); do
        local hue=$((i * 45))
        magick -size 128x128 "xc:hsl($hue,100%,50%)" \
            "$ASSET_DIR/frame_$i.png"
        frames+=("$ASSET_DIR/frame_$i.png")
    done
    magick -delay 15 -loop 0 "${frames[@]}" "$ASSET_DIR/anim.gif"
    rm -f "$ASSET_DIR"/frame_*.png
    ok "anim.gif (128×128, 8 frames, 150ms/frame)"

    # 7. RGBA with transparency (checkerboard + semi-transparent overlay).
    magick -size 128x128 pattern:checkerboard \
        \( -size 128x128 xc:'rgba(255,0,0,0.5)' \) \
        -composite "$ASSET_DIR/alpha.png"
    ok "alpha.png (128×128 with transparency)"

    # 8. Short video clip (3 seconds) for mpv test.
    #    Uses testsrc2 (always available) — drawtext is not in brew ffmpeg.
    if command -v ffmpeg &>/dev/null; then
        if ffmpeg -y -loglevel error \
            -f lavfi -i "testsrc2=s=320x240:d=3:rate=30" \
            -c:v libx264 -pix_fmt yuv420p -t 3 \
            "$ASSET_DIR/clip.mp4" 2>/dev/null; then
            ok "clip.mp4 (320×240, 3s test pattern)"
        else
            warn "ffmpeg failed to generate clip.mp4 — skipping video asset"
        fi
    else
        warn "ffmpeg not found — skipping video asset"
    fi

    echo
}

# ── Demo: Static Images ───────────────────────────────────────────────

demo_static() {
    banner "Static Image Display" "timg -p kitty — tests transmit+display, PNG decode, RGBA"

    section "timg: gradient (256×256)"
    timg -p kitty -g 40x20 "$ASSET_DIR/gradient.png"; reset_term
    ok "gradient.png displayed via timg -p kitty"

    section "timg: plasma fractal (512×384)"
    timg -p kitty -g 60x20 "$ASSET_DIR/plasma.png"; reset_term
    ok "plasma.png displayed — exercises chunked transfer for larger images"

    section "timg: tiny icon (16×16)"
    timg -p kitty "$ASSET_DIR/icon16.png"; reset_term
    ok "icon16.png — small image, minimal cells"

    section "timg: alpha transparency"
    timg -p kitty -g 30x15 "$ASSET_DIR/alpha.png"; reset_term
    ok "alpha.png — RGBA with semi-transparent red overlay"

    section "chafa: plasma (kitty mode)"
    chafa -f kitty --size 60x20 "$ASSET_DIR/plasma.png"; reset_term
    ok "plasma.png via chafa -f kitty"

    section "chafa: gradient (kitty mode)"
    chafa -f kitty --size 40x20 "$ASSET_DIR/gradient.png"; reset_term
    ok "gradient.png via chafa -f kitty"

    pause
}

# ── Demo: Scaling Comparison ──────────────────────────────────────────

demo_scaling() {
    banner "Scaling Test" "Same image at multiple sizes — cell-based scaling (c=/r=)"

    local img="$ASSET_DIR/plasma.png"

    section "timg: thumbnail (15 cols)"
    timg -p kitty -g 15x8 "$img"; reset_term
    ok "15×8 cells (small thumbnail)"

    section "timg: medium (40 cols)"
    timg -p kitty -g 40x20 "$img"; reset_term
    ok "40×20 cells (medium)"

    section "timg: large (70 cols)"
    timg -p kitty -g 70x25 "$img"; reset_term
    ok "70×25 cells (large)"

    section "timg: wide banner (full width)"
    timg -p kitty "$ASSET_DIR/banner.png"; reset_term
    ok "banner.png — tests wide image clipping"

    section "timg: tall strip (capped height)"
    timg -p kitty -g 15x20 "$ASSET_DIR/tall.png"; reset_term
    ok "tall.png at 15×20 — tests tall image scaling"

    section "chafa: side-by-side sizes"
    printf "  "
    chafa -f kitty --size 10x5 "$img"; reset_term
    printf "  "
    chafa -f kitty --size 20x10 "$img"; reset_term
    printf "  "
    chafa -f kitty --size 30x15 "$img"; reset_term
    echo
    ok "chafa at 10×5, 20×10, 30×15 — visual scaling comparison"

    pause
}

# ── Demo: Animated GIF ────────────────────────────────────────────────

demo_anim() {
    banner "Animated GIF" "timg handles frame loops itself — tests rapid transmit+display"

    section "timg: animated GIF (8 frames, 3 loops)"
    info "Playing 3 loops of color-cycling animation..."
    info "Each frame is a full transmit+display — exercises the hot path."
    echo
    timg -p kitty --loops=3 -g 30x15 "$ASSET_DIR/anim.gif"; reset_term
    ok "anim.gif played 3 loops via timg"
    info "(timg sends each frame as a new image — NOT kitty animation protocol)"

    if command -v chafa &>/dev/null; then
        section "chafa: animated GIF"
        info "chafa plays GIFs natively in kitty mode too."
        echo
        chafa -f kitty --size 30x15 --duration 3 "$ASSET_DIR/anim.gif" || true; reset_term
        ok "anim.gif via chafa (duration-limited)"
    fi

    pause
}

# ── Demo: Video Playback ─────────────────────────────────────────────

demo_video() {
    banner "Video Playback" "mpv --vo=kitty and timg video — real-time frame streaming"

    warn "⚠  Video playback sends images at high frame rates."
    warn "   This can trigger segfaults in the graphics rendering pipeline"
    warn "   (a pre-existing issue in the sixel GPU upload path, not kitty-specific)."
    echo
    printf "${DIM}  Run video demos? [y/N] ${RESET}"
    read -r answer
    if [[ "${answer,,}" != "y" ]]; then
        info "Skipped video demos."
        return
    fi

    if [[ ! -f "$ASSET_DIR/clip.mp4" ]]; then
        warn "clip.mp4 not generated (ffmpeg missing?) — skipping video demos"
        return
    fi

    section "timg: video playback (3s clip)"
    info "timg decodes video frames and sends each as a kitty image."
    info "This is the heaviest real-world kitty graphics workload."
    echo
    timg -p kitty -g 40x20 --loops=1 -V "$ASSET_DIR/clip.mp4" || warn "timg exited unexpectedly"; reset_term
    ok "clip.mp4 played via timg -V"

    section "mpv: video playback with --vo=kitty (3s clip)"
    info "mpv has native kitty graphics protocol output."
    info "This tests the full protocol under a demanding client."
    echo
    # mpv --vo=kitty uses the kitty protocol directly.
    # --really-quiet suppresses mpv's status line.
    # --no-terminal prevents mpv from grabbing the terminal.
    mpv --vo=kitty --really-quiet --no-input-terminal \
        --loop-file=no --frames=90 \
        "$ASSET_DIR/clip.mp4" 2>/dev/null || warn "mpv exited (may need terminal focus)"; reset_term
    ok "clip.mp4 played via mpv --vo=kitty"

    pause
}

# ── Demo: Tool Comparison ─────────────────────────────────────────────

demo_tools() {
    banner "Tool Comparison" "Same image rendered by timg vs chafa in kitty mode"

    local img="$ASSET_DIR/plasma.png"
    local size="40x15"

    section "timg -p kitty (--grid ${size})"
    timg -p kitty -g "$size" "$img"; reset_term

    section "chafa -f kitty (--size ${size})"
    chafa -f kitty --size "$size" "$img"; reset_term

    info "Both should look similar. Differences come from:"
    info "  • How each tool maps pixels to cells"
    info "  • Scaling algorithm (timg vs chafa internals)"
    info "  • Whether they use c=/r= (cell scaling) vs pre-scaling pixels"

    pause
}

# ── Demo: Stress Test ─────────────────────────────────────────────────

demo_stress() {
    banner "Stress Test" "Many images, large images, rapid display"

    section "Rapid-fire: 20 small images in sequence"
    info "Tests image ID cycling, memory quota, cleanup."
    for i in $(seq 1 20); do
        local hue=$(( (i * 18) % 360 ))
        magick -size 32x32 "xc:hsl($hue,80%,60%)" "$ASSET_DIR/stress_$i.png"
    done
    for i in $(seq 1 20); do
        timg -p kitty -g 4x2 "$ASSET_DIR/stress_$i.png"; reset_term
        printf " "
    done
    echo
    ok "20 images displayed in rapid succession"

    section "Large image: 1920×1080 (full HD)"
    info "Tests chunked transfer with large payloads."
    magick -size 1920x1080 plasma:fractal "$ASSET_DIR/large.png" 2>/dev/null
    ok "Generated 1920×1080 plasma"
    timg -p kitty -g 70x25 "$ASSET_DIR/large.png"; reset_term
    ok "Displayed at 70×25 cells — large image scaled down"

    section "Grid of images"
    info "Multiple images sharing the terminal grid."
    for i in $(seq 1 6); do
        timg -p kitty -g 12x6 "$ASSET_DIR/gradient.png"; reset_term
    done
    ok "6 copies of gradient in grid layout"

    pause
}

# ── Demo: File Medium ─────────────────────────────────────────────────

demo_file_medium() {
    banner "File Medium (t=f)" "timg can use file transmission when available"

    section "timg with explicit kitty protocol"
    info "Displaying a local file — timg may use t=f (file medium)"
    info "or t=d (direct) depending on its heuristics."
    info "Check alacritty debug log for: [kitty] command: ... medium=File"
    echo
    timg -p kitty -g 40x20 "$ASSET_DIR/plasma.png"; reset_term
    ok "Image displayed — check -vv log for medium used"
    info "Run alacritty with RUST_LOG=debug to see medium in log output."

    pause
}

# ── Main ──────────────────────────────────────────────────────────────

main() {
    banner "Kitty Graphics Protocol — Real-World Demo" \
           "Testing alacritty-kitty with timg, chafa, mpv"

    section "Checking prerequisites"
    local missing=0
    check_tool timg   || missing=$((missing + 1))
    check_tool chafa  || missing=$((missing + 1))
    check_tool mpv    || missing=$((missing + 1))
    check_tool magick || missing=$((missing + 1))
    echo

    if [[ $missing -gt 0 ]]; then
        warn "$missing tool(s) missing. Install with:"
        info "  brew install timg chafa mpv imagemagick"
        echo
        info "Continuing with available tools..."
        echo
    fi

    if ! command -v magick &>/dev/null; then
        fail "ImageMagick (magick) is required to generate test assets."
        fail "Install with: brew install imagemagick"
        exit 1
    fi

    generate_assets

    local cmd="${1:-all}"
    case "$cmd" in
        static)      demo_static ;;
        scaling)     demo_scaling ;;
        anim)        demo_anim ;;
        video)       demo_video ;;
        tools)       demo_tools ;;
        stress)      demo_stress ;;
        file_medium) demo_file_medium ;;
        all)
            demo_static
            demo_scaling
            demo_anim
            demo_video
            demo_tools
            demo_stress
            ;;
        *)
            echo "Usage: $0 [static|scaling|anim|video|tools|stress|file_medium|all]"
            exit 1
            ;;
    esac

    banner "Demo Complete" \
           "If you saw images above, the kitty graphics protocol is working!"

    info "For debugging, relaunch alacritty with:"
    info "  RUST_LOG=debug ./target/debug/alacritty 2>&1 | grep kitty"
    echo
}

main "$@"