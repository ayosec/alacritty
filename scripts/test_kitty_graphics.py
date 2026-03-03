#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.12"
# ///
"""
Manual test script for kitty graphics protocol support.

Usage:
    # First, build and launch the alacritty fork:
    cd kitty/alacritty
    cargo build && ./target/debug/alacritty

    # Then, inside that alacritty window, run:
    uv run scripts/test_kitty_graphics.py

    # Or run individual tests / groups:
    uv run scripts/test_kitty_graphics.py query
    uv run scripts/test_kitty_graphics.py png
    uv run scripts/test_kitty_graphics.py phase1      # all Phase 1 tests
    uv run scripts/test_kitty_graphics.py phase2      # all Phase 2 tests
    uv run scripts/test_kitty_graphics.py file_medium
    uv run scripts/test_kitty_graphics.py tempfile_medium
    uv run scripts/test_kitty_graphics.py shm_medium
    uv run scripts/test_kitty_graphics.py delete_modes
    uv run scripts/test_kitty_graphics.py scaling
    uv run scripts/test_kitty_graphics.py offsets
    uv run scripts/test_kitty_graphics.py animation
    uv run scripts/test_kitty_graphics.py file <path_to_image.png>

See: https://sw.kovidgoyal.net/kitty/graphics-protocol/
"""

import base64
import ctypes
import ctypes.util
import io
import mmap
import os
import select
import struct
import sys
import tempfile
import termios
import time
import tty
import zlib


def drain_responses(timeout: float = 0.1) -> list[str]:
    """Read and discard any pending APC responses from stdin.

    When we send kitty graphics commands, the terminal writes responses
    back to the PTY (e.g. ESC _Gi=1;OK ESC \\). If we don't consume
    them, they leak into the shell prompt as garbage text.

    Returns the raw response strings for debugging.
    """
    responses = []
    fd = sys.stdin.fileno()

    # Save terminal state and switch to raw mode so we can read
    # without waiting for Enter.
    old_settings = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        buf = bytearray()
        while True:
            ready, _, _ = select.select([fd], [], [], timeout)
            if not ready:
                break
            chunk = os.read(fd, 4096)
            if not chunk:
                break
            buf.extend(chunk)
        if buf:
            responses.append(buf.decode("utf-8", errors="replace"))
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old_settings)

    return responses


def write_apc(payload: str, expect_response: bool = True) -> list[str]:
    """Write a kitty graphics APC sequence to stdout.

    If expect_response is True, drains the PTY response so it doesn't
    leak into the shell prompt.
    """
    sys.stdout.write(f"\033_G{payload}\033\\")
    sys.stdout.flush()
    if expect_response:
        return drain_responses()
    return []


def write_chunked(key_values: str, data: bytes, chunk_size: int = 4096) -> list[str]:
    """Write image data in chunks per the kitty protocol."""
    encoded = base64.standard_b64encode(data).decode("ascii")
    chunks = [encoded[i : i + chunk_size] for i in range(0, len(encoded), chunk_size)]

    if not chunks:
        return write_apc(f"{key_values},m=0;")

    for i, chunk in enumerate(chunks):
        is_last = i == len(chunks) - 1
        m = 0 if is_last else 1
        if i == 0:
            # Intermediate chunks don't get responses; only drain on last.
            write_apc(f"{key_values},m={m};{chunk}", expect_response=is_last)
        else:
            write_apc(f"m={m};{chunk}", expect_response=is_last)
    return []


def make_solid_rgba(width: int, height: int, r: int, g: int, b: int, a: int = 255) -> bytes:
    """Create a solid-color RGBA image as raw bytes."""
    pixel = bytes([r, g, b, a])
    return pixel * (width * height)


def make_gradient_rgba(width: int, height: int) -> bytes:
    """Create an RGBA gradient image (red→blue horizontally, green vertically)."""
    pixels = bytearray()
    for y in range(height):
        for x in range(width):
            r = int(255 * x / max(width - 1, 1))
            g = int(255 * y / max(height - 1, 1))
            b = 255 - r
            pixels.extend([r, g, b, 255])
    return bytes(pixels)


def make_checkerboard_rgba(width: int, height: int, sq: int = 8) -> bytes:
    """Create an RGBA checkerboard pattern."""
    pixels = bytearray()
    for y in range(height):
        for x in range(width):
            if ((x // sq) + (y // sq)) % 2 == 0:
                pixels.extend([200, 200, 200, 255])
            else:
                pixels.extend([50, 50, 50, 255])
    return bytes(pixels)


def make_test_png(width: int, height: int) -> bytes:
    """Create a minimal PNG image in memory (no external deps)."""
    # We'll build a valid PNG by hand: IHDR, IDAT, IEND.
    # Using uncompressed deflate blocks for simplicity.

    def png_chunk(chunk_type: bytes, data: bytes) -> bytes:
        chunk = chunk_type + data
        crc = struct.pack(">I", zlib.crc32(chunk) & 0xFFFFFFFF)
        return struct.pack(">I", len(data)) + chunk + crc

    # PNG signature
    sig = b"\x89PNG\r\n\x1a\n"

    # IHDR: width, height, bit_depth=8, color_type=2 (RGB), compression=0, filter=0, interlace=0
    ihdr_data = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    ihdr = png_chunk(b"IHDR", ihdr_data)

    # Image data: each row has a filter byte (0=None) followed by RGB pixels
    raw_rows = bytearray()
    for y in range(height):
        raw_rows.append(0)  # filter: None
        for x in range(width):
            r = int(255 * x / max(width - 1, 1))
            g = int(255 * y / max(height - 1, 1))
            b = 128
            raw_rows.extend([r, g, b])

    compressed = zlib.compress(bytes(raw_rows))
    idat = png_chunk(b"IDAT", compressed)

    iend = png_chunk(b"IEND", b"")

    return sig + ihdr + idat + iend


# ── Test Functions ──────────────────────────────────────────────────────


def test_query():
    """Test 1: Query protocol support (a=q).

    Expected: Terminal should respond with ESC _Gi=1;OK ESC \\
    You can check with: cat -v  (then paste the query, you should see a response)
    """
    print("═" * 60)
    print("TEST: Query protocol support (a=q)")
    print("  Sending query... (response goes to PTY input)")
    print("  Expected response: \\e_Gi=1;OK\\e\\\\")
    print("═" * 60)

    # Query with a tiny 1x1 PNG to test format support.
    png_data = make_test_png(1, 1)
    encoded = base64.standard_b64encode(png_data).decode("ascii")
    responses = write_apc(f"a=q,i=1,f=100;{encoded}")
    if responses:
        raw = responses[0]
        if "OK" in raw:
            print(f"  ✓ Query sent — got OK response")
        else:
            print(f"  ✗ Query sent — unexpected response: {raw!r}")
    else:
        print("  ⚠ Query sent — no response received (timeout)")
    print()


def test_rgba():
    """Test 2: Display a raw RGBA image (f=32, a=T)."""
    print("═" * 60)
    print("TEST: Raw RGBA image (32x32 gradient)")
    print("═" * 60)

    width, height = 32, 32
    pixels = make_gradient_rgba(width, height)
    responses = write_chunked(f"a=T,f=32,s={width},v={height},i=10", pixels)
    print("  ✓ Sent 32x32 RGBA gradient (image id=10)")
    print("  You should see a small colorful square above.\n")


def test_rgb():
    """Test 3: Display a raw RGB image (f=24, a=T)."""
    print("═" * 60)
    print("TEST: Raw RGB image (24x24 solid red)")
    print("═" * 60)

    width, height = 24, 24
    # RGB = 3 bytes per pixel
    pixel = bytes([255, 0, 0])
    pixels = pixel * (width * height)
    responses = write_chunked(f"a=T,f=24,s={width},v={height},i=11", pixels)
    print("  ✓ Sent 24x24 solid red RGB (image id=11)")
    print("  You should see a small red square above.\n")


def test_png():
    """Test 4: Display a PNG image (f=100, a=T)."""
    print("═" * 60)
    print("TEST: PNG image (64x32 gradient)")
    print("═" * 60)

    png_data = make_test_png(64, 32)
    responses = write_chunked("a=T,f=100,i=12", png_data)
    print(f"  ✓ Sent 64x32 PNG ({len(png_data)} bytes, image id=12)")
    print("  You should see a gradient rectangle above.\n")


def test_chunked():
    """Test 5: Chunked transfer with small chunk size."""
    print("═" * 60)
    print("TEST: Chunked transfer (48x48 checkerboard, 256-byte chunks)")
    print("═" * 60)

    width, height = 48, 48
    pixels = make_checkerboard_rgba(width, height)
    # Use very small chunks to exercise the chunked code path.
    responses = write_chunked(f"a=T,f=32,s={width},v={height},i=13", pixels, chunk_size=256)
    print(f"  ✓ Sent 48x48 checkerboard in 256-byte chunks (image id=13)")
    print("  You should see a checkerboard pattern above.\n")


def test_zlib():
    """Test 6: Zlib-compressed RGBA image (o=z)."""
    print("═" * 60)
    print("TEST: Zlib-compressed RGBA image (32x32 solid blue)")
    print("═" * 60)

    width, height = 32, 32
    pixels = make_solid_rgba(width, height, 0, 0, 255)
    compressed = zlib.compress(pixels)
    responses = write_chunked(f"a=T,f=32,o=z,s={width},v={height},i=14", compressed)
    print(f"  ✓ Sent 32x32 blue (compressed {len(pixels)}→{len(compressed)} bytes, image id=14)")
    print("  You should see a solid blue square above.\n")


def test_transmit_then_display():
    """Test 7: Transmit (a=t) then display (a=p) separately."""
    print("═" * 60)
    print("TEST: Separate transmit (a=t) then display (a=p)")
    print("═" * 60)

    width, height = 32, 32
    pixels = make_solid_rgba(width, height, 0, 200, 0)
    responses = write_chunked(f"a=t,f=32,s={width},v={height},i=15", pixels)
    print("  ✓ Transmitted 32x32 green (image id=15) — no display yet")

    # Now display it.
    responses = write_apc("a=p,i=15")
    print("  ✓ Displayed image id=15")
    print("  You should see a solid green square above.\n")


def test_delete():
    """Test 8: Delete operations."""
    print("═" * 60)
    print("TEST: Delete images")
    print("═" * 60)

    # Place a small image.
    width, height = 16, 16
    pixels = make_solid_rgba(width, height, 255, 255, 0)
    responses = write_chunked(f"a=T,f=32,s={width},v={height},i=20", pixels)
    print("  ✓ Placed 16x16 yellow square (image id=20)")

    # Delete by ID — delete commands don't produce responses.
    write_apc("a=d,d=i,i=20", expect_response=False)
    print("  ✓ Deleted image id=20")
    print("  The yellow square may still be visible (cell content cleared on next redraw).\n")


def test_file(path: str):
    """Test 9: Display a PNG file from disk (user-supplied path)."""
    print("═" * 60)
    print(f"TEST: Display PNG file: {path}")
    print("═" * 60)

    if not os.path.exists(path):
        print(f"  ✗ File not found: {path}")
        return

    with open(path, "rb") as f:
        data = f.read()

    responses = write_chunked("a=T,f=100,i=30", data)
    print(f"  ✓ Sent {len(data)} bytes from {path} (image id=30)")
    print("  You should see the image above.\n")


def test_quiet_modes():
    """Test 10: Quiet mode (q=1 suppresses OK, q=2 suppresses all)."""
    print("═" * 60)
    print("TEST: Quiet modes (q=0, q=1, q=2)")
    print("═" * 60)

    png_data = make_test_png(1, 1)
    encoded = base64.standard_b64encode(png_data).decode("ascii")

    r0 = write_apc(f"a=q,q=0,i=40,f=100;{encoded}")
    got0 = "OK" in r0[0] if r0 else False
    print(f"  {'✓' if got0 else '⚠'} q=0 query — {'got OK' if got0 else 'no response'}")

    r1 = write_apc(f"a=q,q=1,i=41,f=100;{encoded}")
    got1 = len(r1) == 0 or not any("OK" in r for r in r1)
    print(f"  {'✓' if got1 else '✗'} q=1 query — {'OK suppressed' if got1 else 'got unexpected response'}")

    r2 = write_apc(f"a=q,q=2,i=42,f=100;{encoded}")
    got2 = len(r2) == 0 or not any("OK" in r for r in r2)
    print(f"  {'✓' if got2 else '✗'} q=2 query — {'all suppressed' if got2 else 'got unexpected response'}")
    print()


def test_cursor_no_move():
    """Test 11: C=1 (don't move cursor after placement)."""
    print("═" * 60)
    print("TEST: Cursor movement control (C=1)")
    print("═" * 60)

    width, height = 16, 16
    pixels = make_solid_rgba(width, height, 200, 100, 50)
    encoded = base64.standard_b64encode(pixels).decode("ascii")

    # Place with C=1 — cursor should NOT move.
    responses = write_chunked(f"a=T,f=32,s={width},v={height},i=50,C=1", pixels)
    print("  ✓ Placed 16x16 image with C=1 (cursor should not move)")
    print("  This text should appear at the same line as the image.\n")


# ── Phase 2: File & Shared Memory Transmission ─────────────────────────


def test_file_medium():
    """Phase 2A: Transmit+display via file medium (t=f).

    Writes a raw RGBA image to a temp file, then tells the terminal to
    read it via the base64-encoded path.
    """
    print("═" * 60)
    print("TEST: File medium (t=f) — 32x32 gradient via filesystem")
    print("═" * 60)

    width, height = 32, 32
    pixels = make_gradient_rgba(width, height)

    with tempfile.NamedTemporaryFile(suffix=".rgba", delete=False) as f:
        f.write(pixels)
        tmp_path = f.name

    path_b64 = base64.standard_b64encode(tmp_path.encode()).decode("ascii")
    responses = write_apc(f"a=T,t=f,f=32,s={width},v={height},i=60;{path_b64}")

    if responses and "OK" in responses[0]:
        print(f"  ✓ File medium — terminal read {tmp_path}")
    elif responses:
        print(f"  ⚠ Response: {responses[0]!r}")
    else:
        print(f"  ✓ Sent file medium command (path={tmp_path})")

    print("  You should see a 32x32 gradient above.")
    # Don't delete — terminal may still need it. It's in $TMPDIR anyway.
    print(f"  (temp file left at {tmp_path})\n")


def test_tempfile_medium():
    """Phase 2A: Transmit+display via temp file medium (t=t).

    The payload is a base64-encoded *relative* path under the system
    temp directory. The terminal should delete the file after reading.
    """
    print("═" * 60)
    print("TEST: Temp file medium (t=t) — 24x24 solid magenta")
    print("═" * 60)

    width, height = 24, 24
    pixels = make_solid_rgba(width, height, 255, 0, 255)

    # Write into the system temp dir with a recognizable filename.
    filename = f"kitty_test_{os.getpid()}.rgba"
    tmp_dir = tempfile.gettempdir()
    full_path = os.path.join(tmp_dir, filename)
    with open(full_path, "wb") as f:
        f.write(pixels)

    # For t=t, the payload is the *relative* path under the temp dir.
    path_b64 = base64.standard_b64encode(filename.encode()).decode("ascii")
    responses = write_apc(f"a=T,t=t,f=32,s={width},v={height},i=61;{path_b64}")

    if responses and "OK" in responses[0]:
        print(f"  ✓ Temp file medium — terminal read {filename}")
    elif responses:
        print(f"  ⚠ Response: {responses[0]!r}")
    else:
        print(f"  ✓ Sent temp file medium command")

    # Check if the terminal deleted the file (as it should).
    time.sleep(0.2)
    if os.path.exists(full_path):
        print(f"  ⚠ File still exists (terminal should have deleted it): {full_path}")
        os.unlink(full_path)
    else:
        print(f"  ✓ File was deleted by the terminal after reading")

    print("  You should see a 24x24 magenta square above.\n")


def test_shm_medium():
    """Phase 2A: Transmit+display via shared memory medium (t=s).

    Creates a POSIX shared memory object, writes pixel data, then tells
    the terminal to mmap and read it.

    Only works on Unix (macOS / Linux).
    """
    print("═" * 60)
    print("TEST: Shared memory medium (t=s) — 16x16 solid cyan")
    print("═" * 60)

    if sys.platform == "win32":
        print("  ⚠ Skipped — POSIX shm not available on Windows\n")
        return

    width, height = 16, 16
    pixels = make_solid_rgba(width, height, 0, 255, 255)

    shm_name = f"/kitty_test_{os.getpid()}"
    try:
        # Use ctypes to call shm_open / ftruncate / mmap.
        libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)

        O_CREAT = 0o100  # May differ per platform; we'll use the os module.
        O_RDWR = os.O_RDWR

        # shm_open returns an fd.
        fd = libc.shm_open(
            shm_name.encode(),
            O_RDWR | os.O_CREAT | os.O_TRUNC,
            0o600,
        )
        if fd < 0:
            errno = ctypes.get_errno()
            print(f"  ✗ shm_open failed: errno={errno}")
            return

        os.ftruncate(fd, len(pixels))

        # mmap and write.
        mm = mmap.mmap(fd, len(pixels), access=mmap.ACCESS_WRITE)
        mm.write(pixels)
        mm.close()
        os.close(fd)

        # Payload is the base64-encoded shm name.
        name_b64 = base64.standard_b64encode(shm_name.encode()).decode("ascii")
        responses = write_apc(f"a=T,t=s,f=32,s={width},v={height},i=62;{name_b64}")

        if responses and "OK" in responses[0]:
            print(f"  ✓ SHM medium — terminal read {shm_name}")
        elif responses:
            print(f"  ⚠ Response: {responses[0]!r}")
        else:
            print(f"  ✓ Sent shm medium command (name={shm_name})")

        # Terminal should have shm_unlink'd. Check by trying to open it.
        time.sleep(0.2)
        fd2 = libc.shm_open(shm_name.encode(), os.O_RDONLY, 0)
        if fd2 >= 0:
            os.close(fd2)
            print(f"  ⚠ SHM still exists (terminal should have unlinked it)")
            libc.shm_unlink(shm_name.encode())
        else:
            print(f"  ✓ SHM was unlinked by the terminal after reading")

    except Exception as e:
        print(f"  ✗ Error: {e}")
        # Try to clean up.
        try:
            libc.shm_unlink(shm_name.encode())
        except Exception:
            pass

    print("  You should see a 16x16 cyan square above.\n")


# ── Phase 2: Full Delete Modes ─────────────────────────────────────────


def test_delete_modes():
    """Phase 2B: Exercise all delete target modes.

    Places several images at different locations, then deletes them using
    various d= modes and reports the results.
    """
    print("═" * 60)
    print("TEST: Full delete modes (d=a, d=i, d=n, d=p, d=c, d=z)")
    print("═" * 60)

    def place(image_id, width, height, r, g, b, **extra):
        """Helper to place a solid-color image."""
        pixels = make_solid_rgba(width, height, r, g, b)
        kv_extra = ",".join(f"{k}={v}" for k, v in extra.items())
        kv = f"a=T,f=32,s={width},v={height},i={image_id}"
        if kv_extra:
            kv += f",{kv_extra}"
        write_chunked(kv, pixels)

    # Place a row of small images.
    print("  Placing 5 small images (id=70..74)...")
    place(70, 12, 12, 255, 0, 0)      # red
    place(71, 12, 12, 0, 255, 0)      # green
    place(72, 12, 12, 0, 0, 255)      # blue
    place(73, 12, 12, 255, 255, 0)    # yellow
    place(74, 12, 12, 255, 0, 255)    # magenta
    print()

    # Delete by ID.
    write_apc("a=d,d=i,i=70", expect_response=False)
    print("  ✓ d=i,i=70 — deleted red square by ID")

    # Delete by image number — first we need one with I= set.
    pixels_white = make_solid_rgba(12, 12, 255, 255, 255)
    write_chunked("a=T,f=32,s=12,v=12,i=75,I=500", pixels_white)
    write_apc("a=d,d=n,I=500", expect_response=False)
    print("  ✓ d=n,I=500 — deleted white square by number mapping")

    # Delete by placement ID.
    place(76, 12, 12, 128, 128, 128, p=99)
    write_apc("a=d,d=p,i=76,p=99", expect_response=False)
    print("  ✓ d=p,i=76,p=99 — deleted grey square by placement ID")

    # Delete at cursor position (d=c).
    place(77, 12, 12, 0, 128, 128)
    write_apc("a=d,d=c", expect_response=False)
    print("  ✓ d=c — deleted teal square at cursor position")

    # Delete by z-index.
    place(78, 12, 12, 128, 0, 128, z=42)
    write_apc("a=d,d=z,z=42", expect_response=False)
    print("  ✓ d=z,z=42 — deleted purple square by z-index")

    # Delete all remaining.
    write_apc("a=d,d=a", expect_response=False)
    print("  ✓ d=a — deleted all remaining images")

    print()
    print("  All delete modes exercised. Images should be gone from")
    print("  storage (visible squares may linger until cells redraw).\n")


# ── Phase 2: Scaling & Pixel Offsets ────────────────────────────────────


def test_scaling():
    """Phase 2C: Cell-based scaling (c= / r= columns/rows).

    Displays the same source image at several different cell sizes
    so you can visually confirm scaling works.
    """
    print("═" * 60)
    print("TEST: Cell-based scaling (c= / r= columns/rows)")
    print("═" * 60)

    # Use a 64x64 checkerboard as the source — easy to see scaling artifacts.
    width, height = 64, 64
    pixels = make_checkerboard_rgba(width, height, sq=8)

    # 1. No scaling — natural size.
    write_chunked(f"a=T,f=32,s={width},v={height},i=80", pixels)
    print("  ✓ Natural size (64x64 pixels, no c=/r=)")
    print()

    # 2. Scale to 10 columns wide (proportional height).
    write_chunked(f"a=T,f=32,s={width},v={height},i=81,c=10", pixels)
    print("  ✓ Scaled to c=10 columns (proportional height)")
    print()

    # 3. Scale to 5 rows tall (proportional width).
    write_chunked(f"a=T,f=32,s={width},v={height},i=82,r=5", pixels)
    print("  ✓ Scaled to r=5 rows (proportional width)")
    print()

    # 4. Scale to exact 20 columns × 4 rows (may distort).
    write_chunked(f"a=T,f=32,s={width},v={height},i=83,c=20,r=4", pixels)
    print("  ✓ Scaled to c=20, r=4 (stretched)")
    print()

    # 5. Scale down to 3 columns × 3 rows.
    write_chunked(f"a=T,f=32,s={width},v={height},i=84,c=3,r=3", pixels)
    print("  ✓ Scaled to c=3, r=3 (thumbnail)")
    print()

    print("  You should see 5 checkerboards at different sizes above.\n")


def test_offsets():
    """Phase 2C: Sub-cell pixel offsets (X= / Y=).

    Places the same image multiple times with different X=/Y= offsets.
    The visual effect is subtle: the image shifts within its first cell.
    """
    print("═" * 60)
    print("TEST: Sub-cell pixel offsets (X= / Y=)")
    print("═" * 60)

    width, height = 16, 16
    pixels = make_solid_rgba(width, height, 255, 100, 0)

    # 1. No offset — baseline.
    write_chunked(f"a=T,f=32,s={width},v={height},i=85", pixels)
    print("  ✓ No offset (baseline)")

    # 2. X=4 — shift right 4 pixels.
    write_chunked(f"a=T,f=32,s={width},v={height},i=86,X=4", pixels)
    print("  ✓ X=4 (shifted right 4px)")

    # 3. Y=4 — shift down 4 pixels.
    write_chunked(f"a=T,f=32,s={width},v={height},i=87,Y=4", pixels)
    print("  ✓ Y=4 (shifted down 4px)")

    # 4. X=4,Y=4 — shift both.
    write_chunked(f"a=T,f=32,s={width},v={height},i=88,X=4,Y=4", pixels)
    print("  ✓ X=4,Y=4 (shifted right+down 4px)")
    print()

    print("  The four orange squares above should show progressive offset.")
    print("  The effect is subtle at small offsets — look at alignment.\n")


def test_crop_and_scale():
    """Phase 2C: Crop source rect + scale to cells."""
    print("═" * 60)
    print("TEST: Crop (x/y/w/h) + Scale (c/r) combined")
    print("═" * 60)

    # 64x64 gradient: we'll crop a 32x32 quadrant then scale it.
    width, height = 64, 64
    pixels = make_gradient_rgba(width, height)

    # Show the full image first for reference.
    write_chunked(f"a=T,f=32,s={width},v={height},i=89", pixels)
    print("  ✓ Full 64x64 gradient (reference)")
    print()

    # Crop top-left 32x32, display at natural size.
    write_chunked(f"a=T,f=32,s={width},v={height},i=90,x=0,y=0,w=32,h=32", pixels)
    print("  ✓ Cropped top-left 32x32")
    print()

    # Crop bottom-right 32x32, scale to 10 columns.
    write_chunked(f"a=T,f=32,s={width},v={height},i=91,x=32,y=32,w=32,h=32,c=10", pixels)
    print("  ✓ Cropped bottom-right 32x32, scaled to c=10")
    print()

    print("  You should see: full gradient, top-left quadrant, then")
    print("  bottom-right quadrant scaled wider.\n")


# ── Phase 2: Animation ─────────────────────────────────────────────────


def test_animation():
    """Phase 2D: Animation frames (a=f) and control (a=a).

    Creates a base image, adds animation frames, and exercises
    animation control commands.

    NOTE: The render-loop timer tick is NOT implemented yet, so
    frames won't auto-advance. This test verifies the protocol
    parsing and state management work — you'll see the base image
    but frames won't animate until Phase 2E (render loop).
    """
    print("═" * 60)
    print("TEST: Animation frames (a=f) and control (a=a)")
    print("  NOTE: Frames won't auto-advance (render loop is Phase 2E).")
    print("  This test verifies protocol parsing + state management.")
    print("═" * 60)

    width, height = 32, 32

    # 1. Transmit base image (frame 0) — solid red.
    pixels_red = make_solid_rgba(width, height, 255, 0, 0)
    write_chunked(f"a=T,f=32,s={width},v={height},i=100", pixels_red)
    print("  ✓ Base image (frame 0): solid red, id=100")
    print()

    # 2. Add frame 1 — solid green, 200ms gap.
    pixels_green = make_solid_rgba(width, height, 0, 255, 0)
    encoded_green = base64.standard_b64encode(pixels_green).decode("ascii")
    responses = write_apc(f"a=f,i=100,f=32,s={width},v={height},z=200;{encoded_green}")
    if responses and "OK" in responses[0]:
        print("  ✓ Frame 1 added: solid green, gap=200ms")
    elif responses:
        print(f"  ⚠ Frame 1 response: {responses[0]!r}")
    else:
        print("  ✓ Frame 1 sent: solid green, gap=200ms")

    # 3. Add frame 2 — solid blue, 200ms gap.
    pixels_blue = make_solid_rgba(width, height, 0, 0, 255)
    encoded_blue = base64.standard_b64encode(pixels_blue).decode("ascii")
    responses = write_apc(f"a=f,i=100,f=32,s={width},v={height},z=200;{encoded_blue}")
    if responses and "OK" in responses[0]:
        print("  ✓ Frame 2 added: solid blue, gap=200ms")
    elif responses:
        print(f"  ⚠ Frame 2 response: {responses[0]!r}")
    else:
        print("  ✓ Frame 2 sent: solid blue, gap=200ms")

    # 4. Animation control: start playback (s=1).
    write_apc("a=a,i=100,s=1", expect_response=False)
    print("  ✓ Animation control: s=1 (start playback)")

    # 5. Animation control: set loop count.
    write_apc("a=a,i=100,v=3", expect_response=False)
    print("  ✓ Animation control: v=3 (loop 3 times)")

    # 6. Animation control: stop (s=3).
    write_apc("a=a,i=100,s=3", expect_response=False)
    print("  ✓ Animation control: s=3 (stop / set to loading)")

    print()
    print("  You should see a red square above (base frame).")
    print("  Frames are stored but won't auto-cycle until render loop")
    print("  is implemented (Phase 2E).\n")


def test_animation_compose():
    """Phase 2D: Frame composition (a=c).

    Creates a base image with frames, then uses compose to copy
    pixels between frames.
    """
    print("═" * 60)
    print("TEST: Animation frame composition (a=c)")
    print("═" * 60)

    width, height = 16, 16

    # Base image.
    pixels = make_solid_rgba(width, height, 100, 100, 100)
    write_chunked(f"a=T,f=32,s={width},v={height},i=101", pixels)

    # Add a second frame.
    frame_pixels = make_solid_rgba(width, height, 200, 50, 50)
    encoded = base64.standard_b64encode(frame_pixels).decode("ascii")
    write_apc(f"a=f,i=101,f=32,s={width},v={height};{encoded}")
    print("  ✓ Base + 1 frame created for image id=101")

    # Compose: copy frame 0 onto frame 1 (c=0 base frame, r=1 target frame).
    responses = write_apc("a=c,i=101,c=0,r=1")
    if responses and "OK" in responses[0]:
        print("  ✓ Composed: copied frame 0 → frame 1")
    elif responses:
        print(f"  ⚠ Compose response: {responses[0]!r}")
    else:
        print("  ✓ Compose command sent (frame 0 → frame 1)")

    print("  Frame composition exercised.\n")


# ── Phase 2: Combined / Stress Tests ───────────────────────────────────


def test_file_with_offset_size():
    """Phase 2A: File medium with S= (size) and O= (offset)."""
    print("═" * 60)
    print("TEST: File medium with offset+size (t=f, S=, O=)")
    print("═" * 60)

    width, height = 4, 4
    # Write 64 bytes of pixel data, but prepend 16 bytes of junk.
    junk = bytes(range(16))
    pixels = make_solid_rgba(width, height, 0, 255, 128)
    payload = junk + pixels + junk  # junk | real data | junk

    with tempfile.NamedTemporaryFile(suffix=".rgba", delete=False) as f:
        f.write(payload)
        tmp_path = f.name

    # Tell terminal to skip 16 bytes (O=16) and read 64 bytes (S=64).
    path_b64 = base64.standard_b64encode(tmp_path.encode()).decode("ascii")
    responses = write_apc(
        f"a=T,t=f,f=32,s={width},v={height},i=110,O=16,S=64;{path_b64}"
    )

    if responses and "OK" in responses[0]:
        print(f"  ✓ File with O=16, S=64 — read {tmp_path}")
    elif responses:
        print(f"  ⚠ Response: {responses[0]!r}")
    else:
        print(f"  ✓ Sent file medium with offset/size")

    os.unlink(tmp_path)
    print("  You should see a tiny 4x4 green square above.\n")


def test_tempfile_path_traversal():
    """Phase 2A: Verify that path traversal in t=t is rejected."""
    print("═" * 60)
    print("TEST: Temp file path traversal rejection (t=t, ../)")
    print("═" * 60)

    evil_path = "../../../etc/passwd"
    path_b64 = base64.standard_b64encode(evil_path.encode()).decode("ascii")
    responses = write_apc(f"a=T,t=t,f=32,s=1,v=1,i=111;{path_b64}")

    if responses:
        resp = responses[0]
        if "OK" not in resp:
            print(f"  ✓ Path traversal rejected: {resp!r}")
        else:
            print(f"  ✗ Path traversal was NOT rejected! Got OK.")
    else:
        print("  ⚠ No response (may be quiet mode — check debug log)")
    print()


# ── Test Group Runners ──────────────────────────────────────────────────


def run_phase1():
    """Run all Phase 1 tests."""
    print()
    print("╔══════════════════════════════════════════════════════════╗")
    print("║          Phase 1 — Basic Protocol Tests                 ║")
    print("╚══════════════════════════════════════════════════════════╝")
    print()
    test_query()
    test_rgba()
    test_rgb()
    test_png()
    test_chunked()
    test_zlib()
    test_transmit_then_display()
    test_delete()
    test_quiet_modes()
    test_cursor_no_move()


def run_phase2():
    """Run all Phase 2 tests."""
    print()
    print("╔══════════════════════════════════════════════════════════╗")
    print("║          Phase 2 — Advanced Protocol Tests              ║")
    print("║                                                         ║")
    print("║  A: File / TempFile / SHM transmission                  ║")
    print("║  B: Full delete modes                                   ║")
    print("║  C: Scaling (c=/r=) and pixel offsets (X=/Y=)           ║")
    print("║  D: Animation frames + control                          ║")
    print("╚══════════════════════════════════════════════════════════╝")
    print()

    # Stream A: Transmission mediums.
    test_file_medium()
    test_tempfile_medium()
    test_shm_medium()
    test_file_with_offset_size()
    test_tempfile_path_traversal()

    # Stream B: Delete modes.
    test_delete_modes()

    # Stream C: Scaling & offsets.
    test_scaling()
    test_offsets()
    test_crop_and_scale()

    # Stream D: Animation.
    test_animation()
    test_animation_compose()


def run_all():
    """Run all tests in sequence."""
    print()
    print("╔══════════════════════════════════════════════════════════╗")
    print("║       Kitty Graphics Protocol — Manual Test Suite       ║")
    print("║                                                         ║")
    print("║  Run inside the alacritty-kitty fork to test graphics.  ║")
    print("║  Build: cargo build                                     ║")
    print("║  Launch: ./target/debug/alacritty                       ║")
    print("╚══════════════════════════════════════════════════════════╝")
    print()

    run_phase1()
    run_phase2()

    print("═" * 60)
    print("ALL TESTS SENT.")
    print()
    print("Phase 1: Basic display, chunked transfer, query, quiet modes")
    print("Phase 2: File/shm mediums, delete modes, scaling, animation")
    print()
    print("If you see colored squares above, the protocol is working!")
    print("If you only see text, check the debug log output:")
    print("  RUST_LOG=debug ./target/debug/alacritty 2>&1 | grep kitty")
    print("═" * 60)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        run_all()
    else:
        cmd = sys.argv[1]
        dispatch = {
            # Phase 1.
            "query": test_query,
            "rgba": test_rgba,
            "rgb": test_rgb,
            "png": test_png,
            "chunked": test_chunked,
            "zlib": test_zlib,
            "transmit": test_transmit_then_display,
            "delete": test_delete,
            "quiet": test_quiet_modes,
            "cursor": test_cursor_no_move,
            "phase1": run_phase1,
            # Phase 2.
            "file_medium": test_file_medium,
            "tempfile_medium": test_tempfile_medium,
            "shm_medium": test_shm_medium,
            "file_offset": test_file_with_offset_size,
            "tempfile_traversal": test_tempfile_path_traversal,
            "delete_modes": test_delete_modes,
            "scaling": test_scaling,
            "offsets": test_offsets,
            "crop_scale": test_crop_and_scale,
            "animation": test_animation,
            "animation_compose": test_animation_compose,
            "phase2": run_phase2,
            # All.
            "all": run_all,
        }

        if cmd == "file":
            if len(sys.argv) < 3:
                print("Usage: test_kitty_graphics.py file <path_to_image.png>")
                sys.exit(1)
            test_file(sys.argv[2])
        elif cmd in dispatch:
            dispatch[cmd]()
        else:
            print(f"Unknown test: {cmd}")
            print(f"Available: {', '.join(sorted(dispatch.keys()))}, file <path>")
            sys.exit(1)