//! Kitty graphics protocol parser.
//!
//! Parses the key=value format used in kitty graphics APC sequences.
//! See: https://sw.kovidgoyal.net/kitty/graphics-protocol/

use log::debug;

/// Action type for the kitty graphics command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `a=t` — Transmit image data (default).
    Transmit,
    /// `a=T` — Transmit and display.
    TransmitAndDisplay,
    /// `a=q` — Query terminal support.
    Query,
    /// `a=p` — Display a previously transmitted image.
    Display,
    /// `a=d` — Delete images/placements.
    Delete,
    /// `a=f` — Transmit animation frame.
    TransmitFrame,
    /// `a=a` — Control animation.
    AnimationControl,
    /// `a=c` — Compose animation frames.
    ComposeFrames,
}

impl Action {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            b't' => Some(Action::Transmit),
            b'T' => Some(Action::TransmitAndDisplay),
            b'q' => Some(Action::Query),
            b'p' => Some(Action::Display),
            b'd' => Some(Action::Delete),
            b'f' => Some(Action::TransmitFrame),
            b'a' => Some(Action::AnimationControl),
            b'c' => Some(Action::ComposeFrames),
            _ => None,
        }
    }
}

/// Image data format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `f=24` — 24-bit RGB.
    Rgb,
    /// `f=32` — 32-bit RGBA (default).
    Rgba,
    /// `f=100` — PNG.
    Png,
}

impl Format {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            24 => Some(Format::Rgb),
            32 => Some(Format::Rgba),
            100 => Some(Format::Png),
            _ => None,
        }
    }
}

/// Transmission medium.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    /// `t=d` — Direct (inline base64, default).
    Direct,
    /// `t=f` — Regular file.
    File,
    /// `t=t` — Temporary file.
    TempFile,
    /// `t=s` — Shared memory.
    SharedMemory,
}

impl Medium {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'd' => Some(Medium::Direct),
            b'f' => Some(Medium::File),
            b't' => Some(Medium::TempFile),
            b's' => Some(Medium::SharedMemory),
            _ => None,
        }
    }
}

/// Compression type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// No compression.
    None,
    /// `o=z` — zlib/deflate.
    Zlib,
}

impl Compression {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'z' => Some(Compression::Zlib),
            _ => None,
        }
    }
}

/// Quiet mode for response suppression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quiet {
    /// `q=0` — Send all responses (default).
    None,
    /// `q=1` — Suppress OK responses.
    SuppressOk,
    /// `q=2` — Suppress all responses.
    SuppressAll,
}

impl Quiet {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Quiet::None),
            1 => Some(Quiet::SuppressOk),
            2 => Some(Quiet::SuppressAll),
            _ => None,
        }
    }
}

/// Delete target for `a=d` commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteTarget {
    /// `d=a` — All visible placements.
    All,
    /// `d=A` — All placements (visible and in scrollback).
    AllIncludingScrollback,
    /// `d=i` — By image ID (visible).
    ById,
    /// `d=I` — By image ID (including scrollback).
    ByIdIncludingScrollback,
    /// `d=n` — By image number (visible).
    ByNumber,
    /// `d=N` — By image number (including scrollback).
    ByNumberIncludingScrollback,
    /// `d=c` — At cursor position (visible).
    AtCursor,
    /// `d=C` — At cursor position (including scrollback).
    AtCursorIncludingScrollback,
    /// `d=f` — Animation frames by image ID.
    AnimationFrames,
    /// `d=F` — Animation frames by image ID (including scrollback).
    AnimationFramesIncludingScrollback,
    /// `d=p` — By placement ID (visible).
    ByPlacementId,
    /// `d=P` — By placement ID (including scrollback).
    ByPlacementIdIncludingScrollback,
    /// `d=q` — By column range (visible).
    ByColumn,
    /// `d=Q` — By column range (including scrollback).
    ByColumnIncludingScrollback,
    /// `d=r` — By row range (visible).
    ByRow,
    /// `d=R` — By row range (including scrollback).
    ByRowIncludingScrollback,
    /// `d=x` — By cell position (visible).
    ByCell,
    /// `d=X` — By cell position (including scrollback).
    ByCellIncludingScrollback,
    /// `d=y` — By cell position with z-index (visible).
    ByCellZ,
    /// `d=Y` — By cell position with z-index (including scrollback).
    ByCellZIncludingScrollback,
    /// `d=z` — By z-index (visible).
    ByZIndex,
    /// `d=Z` — By z-index (including scrollback).
    ByZIndexIncludingScrollback,
}

impl DeleteTarget {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'a' => Some(DeleteTarget::All),
            b'A' => Some(DeleteTarget::AllIncludingScrollback),
            b'i' => Some(DeleteTarget::ById),
            b'I' => Some(DeleteTarget::ByIdIncludingScrollback),
            b'n' => Some(DeleteTarget::ByNumber),
            b'N' => Some(DeleteTarget::ByNumberIncludingScrollback),
            b'c' => Some(DeleteTarget::AtCursor),
            b'C' => Some(DeleteTarget::AtCursorIncludingScrollback),
            b'f' => Some(DeleteTarget::AnimationFrames),
            b'F' => Some(DeleteTarget::AnimationFramesIncludingScrollback),
            b'p' => Some(DeleteTarget::ByPlacementId),
            b'P' => Some(DeleteTarget::ByPlacementIdIncludingScrollback),
            b'q' => Some(DeleteTarget::ByColumn),
            b'Q' => Some(DeleteTarget::ByColumnIncludingScrollback),
            b'r' => Some(DeleteTarget::ByRow),
            b'R' => Some(DeleteTarget::ByRowIncludingScrollback),
            b'x' => Some(DeleteTarget::ByCell),
            b'X' => Some(DeleteTarget::ByCellIncludingScrollback),
            b'y' => Some(DeleteTarget::ByCellZ),
            b'Y' => Some(DeleteTarget::ByCellZIncludingScrollback),
            b'z' => Some(DeleteTarget::ByZIndex),
            b'Z' => Some(DeleteTarget::ByZIndexIncludingScrollback),
            _ => None,
        }
    }
}

/// Parsed kitty graphics command.
///
/// Contains all the fields from a kitty graphics APC sequence. Not all fields
/// are meaningful for every action — they are overloaded based on context.
#[derive(Debug, Clone)]
pub struct KittyCommand {
    /// Action type (`a=`). Default: Transmit.
    pub action: Action,
    /// Quiet mode (`q=`). Default: None.
    pub quiet: Quiet,
    /// Image ID (`i=`).
    pub image_id: u32,
    /// Image number (`I=`).
    pub image_number: u32,
    /// Placement ID (`p=`).
    pub placement_id: u32,
    /// Image data format (`f=`). Default: Rgba.
    pub format: Format,
    /// Transmission medium (`t=`). Default: Direct.
    pub medium: Medium,
    /// Compression (`o=`). Default: None.
    pub compression: Compression,
    /// More chunks follow (`m=`). Default: false.
    pub more_chunks: bool,
    /// Pixel width of source data (`s=`).
    pub width: u32,
    /// Pixel height of source data (`v=`).
    pub height: u32,
    /// Data size in bytes (`S=`).
    pub data_size: u32,
    /// Data offset (`O=`).
    pub data_offset: u32,
    // Display / placement keys:
    /// Source rect x (`x=`).
    pub src_x: u32,
    /// Source rect y (`y=`).
    pub src_y: u32,
    /// Source rect width (`w=`).
    pub src_w: u32,
    /// Source rect height (`h=`).
    pub src_h: u32,
    /// Cell pixel X offset (`X=`).
    pub offset_x: u32,
    /// Cell pixel Y offset (`Y=`).
    pub offset_y: u32,
    /// Display columns (`c=`).
    pub columns: u32,
    /// Display rows (`r=`).
    pub rows: u32,
    /// Z-index (`z=`), signed.
    pub z_index: i32,
    /// Cursor movement control (`C=`). 0=move (default), 1=don't move.
    pub cursor_movement: u32,
    /// Virtual placement (`U=`). 0=false, 1=true.
    pub virtual_placement: u32,
    /// Parent image ID for relative placement (`P=`).
    pub parent_id: u32,
    /// Parent placement ID for relative placement (`Q=`).
    pub parent_placement_id: u32,
    /// Horizontal offset for relative placement (`H=`), signed.
    pub horizontal_offset: i32,
    /// Vertical offset for relative placement (`V=`), signed.
    pub vertical_offset: i32,
    /// Delete target (`d=`).
    pub delete: Option<DeleteTarget>,
    // Animation keys (overloaded):
    /// Animation state for control (`s=` in animation context).
    pub anim_state: u32,
    /// Frame gap in ms (`z=` in animation frame context), signed.
    pub gap_ms: i32,
    /// Composition mode for animation frames (`X=` in frame context).
    pub compose_mode: u32,
    /// Background pixel color for animation (`Y=` in frame context).
    pub background: u32,
    /// Loop count for animation (`v=` in animation control context).
    pub loops: u32,

    /// The raw base64 payload bytes (after `;`), or pre-decoded raw bytes
    /// if `pre_decoded` is true (set by chunked transfer finalization).
    pub payload: Vec<u8>,

    /// When true, `payload` contains already-decoded raw bytes (not base64).
    /// Set by `finalize_chunked` after per-chunk base64 decoding.
    pub pre_decoded: bool,
}

impl Default for KittyCommand {
    fn default() -> Self {
        KittyCommand {
            action: Action::Transmit,
            quiet: Quiet::None,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            format: Format::Rgba,
            medium: Medium::Direct,
            compression: Compression::None,
            more_chunks: false,
            width: 0,
            height: 0,
            data_size: 0,
            data_offset: 0,
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            offset_x: 0,
            offset_y: 0,
            columns: 0,
            rows: 0,
            z_index: 0,
            cursor_movement: 0,
            virtual_placement: 0,
            parent_id: 0,
            parent_placement_id: 0,
            horizontal_offset: 0,
            vertical_offset: 0,
            delete: None,
            anim_state: 0,
            gap_ms: 0,
            compose_mode: 0,
            background: 0,
            loops: 0,
            payload: Vec::new(),
            pre_decoded: false,
        }
    }
}

/// Accumulator for APC bytes. Collects bytes during apc_put and parses
/// at apc_end.
#[derive(Debug, Default)]
pub struct KittyApcBuffer {
    /// Raw bytes accumulated during APC sequence.
    buf: Vec<u8>,
    /// Whether the first byte was 'G' (valid kitty graphics sequence).
    is_kitty: Option<bool>,
}

impl KittyApcBuffer {
    /// Create a new empty APC buffer.
    #[must_use]
    pub fn new() -> Self {
        KittyApcBuffer {
            buf: Vec::with_capacity(8192),
            is_kitty: None,
        }
    }

    /// Reset the buffer for a new APC sequence.
    pub fn start(&mut self) {
        self.buf.clear();
        self.is_kitty = None;
    }

    /// Add a byte to the buffer. Returns false if this is definitely
    /// not a kitty graphics sequence.
    pub fn put(&mut self, byte: u8) -> bool {
        if self.is_kitty == Some(false) {
            return false;
        }
        if self.is_kitty.is_none() {
            // First byte determines if this is a kitty graphics sequence.
            self.is_kitty = Some(byte == b'G');
            if byte != b'G' {
                return false;
            }
            // Don't store the 'G' prefix.
            return true;
        }
        self.buf.push(byte);
        true
    }

    /// Finalize the buffer and parse the command. Returns None if this
    /// was not a kitty graphics sequence or if parsing failed.
    pub fn finish(&mut self) -> Option<KittyCommand> {
        if self.is_kitty != Some(true) {
            return None;
        }
        let result = parse_command(&self.buf);
        self.buf.clear();
        self.is_kitty = None;
        result
    }
}

/// Parse a kitty graphics command from the APC payload (after 'G' prefix).
///
/// Format: `key=value[,key=value...][;base64_payload]`
fn parse_command(data: &[u8]) -> Option<KittyCommand> {
    let mut cmd = KittyCommand::default();

    // Split at first ';' into key-value part and payload.
    let (kv_part, payload) = match data.iter().position(|&b| b == b';') {
        Some(pos) => (&data[..pos], &data[pos + 1..]),
        None => (data, &[] as &[u8]),
    };

    cmd.payload = payload.to_vec();

    // Parse key=value pairs separated by ','.
    if !kv_part.is_empty() {
        let kv_str = std::str::from_utf8(kv_part).ok()?;
        for pair in kv_str.split(',') {
            if pair.is_empty() {
                continue;
            }
            let mut iter = pair.splitn(2, '=');
            let key = iter.next()?;
            let value = iter.next().unwrap_or("");

            // Keys must be single characters.
            if key.len() != 1 {
                debug!("[kitty] ignoring multi-char key: {key}");
                continue;
            }

            let key_byte = key.as_bytes()[0];
            apply_key_value(&mut cmd, key_byte, value);
        }
    }

    Some(cmd)
}

/// Apply a single key=value pair to the command.
fn apply_key_value(cmd: &mut KittyCommand, key: u8, value: &str) {
    match key {
        b'a' => {
            if let Some(b) = value.bytes().next() {
                if let Some(action) = Action::from_byte(b) {
                    cmd.action = action;
                }
            }
        },
        b'q' => {
            if let Ok(v) = value.parse::<u32>() {
                if let Some(quiet) = Quiet::from_u32(v) {
                    cmd.quiet = quiet;
                }
            }
        },
        b'i' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.image_id = v;
            }
        },
        b'I' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.image_number = v;
            }
        },
        b'p' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.placement_id = v;
            }
        },
        b'f' => {
            if let Ok(v) = value.parse::<u32>() {
                if let Some(fmt) = Format::from_u32(v) {
                    cmd.format = fmt;
                }
            }
        },
        b't' => {
            if let Some(b) = value.bytes().next() {
                if let Some(medium) = Medium::from_byte(b) {
                    cmd.medium = medium;
                }
            }
        },
        b'o' => {
            if let Some(b) = value.bytes().next() {
                if let Some(comp) = Compression::from_byte(b) {
                    cmd.compression = comp;
                }
            }
        },
        b'm' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.more_chunks = v > 0;
            }
        },
        b's' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.width = v;
            }
        },
        b'v' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.height = v;
            }
        },
        b'S' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.data_size = v;
            }
        },
        b'O' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.data_offset = v;
            }
        },
        b'x' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.src_x = v;
            }
        },
        b'y' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.src_y = v;
            }
        },
        b'w' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.src_w = v;
            }
        },
        b'h' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.src_h = v;
            }
        },
        b'X' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.offset_x = v;
            }
        },
        b'Y' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.offset_y = v;
            }
        },
        b'c' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.columns = v;
            }
        },
        b'r' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.rows = v;
            }
        },
        b'z' => {
            if let Ok(v) = value.parse::<i32>() {
                cmd.z_index = v;
            }
        },
        b'C' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.cursor_movement = v;
            }
        },
        b'U' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.virtual_placement = v;
            }
        },
        b'P' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.parent_id = v;
            }
        },
        b'Q' => {
            if let Ok(v) = value.parse::<u32>() {
                cmd.parent_placement_id = v;
            }
        },
        b'H' => {
            if let Ok(v) = value.parse::<i32>() {
                cmd.horizontal_offset = v;
            }
        },
        b'V' => {
            if let Ok(v) = value.parse::<i32>() {
                cmd.vertical_offset = v;
            }
        },
        b'd' => {
            if let Some(b) = value.bytes().next() {
                cmd.delete = DeleteTarget::from_byte(b);
            }
        },
        _ => {
            debug!("[kitty] unknown key: {} = {}", key as char, value);
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_transmit_and_display_png() {
        // a=T,f=100,i=1;base64data
        let data = b"a=T,f=100,i=1;AAAA";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.action, Action::TransmitAndDisplay);
        assert_eq!(cmd.format, Format::Png);
        assert_eq!(cmd.image_id, 1);
        assert_eq!(cmd.payload, b"AAAA");
    }

    #[test]
    fn parse_transmit_default_action() {
        // No a= means default Transmit.
        let data = b"f=32,s=100,v=100;payload";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.action, Action::Transmit);
        assert_eq!(cmd.format, Format::Rgba);
        assert_eq!(cmd.width, 100);
        assert_eq!(cmd.height, 100);
        assert_eq!(cmd.payload, b"payload");
    }

    #[test]
    fn parse_query() {
        let data = b"a=q,i=42,f=100;";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.action, Action::Query);
        assert_eq!(cmd.image_id, 42);
        assert_eq!(cmd.format, Format::Png);
    }

    #[test]
    fn parse_display() {
        let data = b"a=p,i=5,p=1,c=10,r=5,z=-1";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.action, Action::Display);
        assert_eq!(cmd.image_id, 5);
        assert_eq!(cmd.placement_id, 1);
        assert_eq!(cmd.columns, 10);
        assert_eq!(cmd.rows, 5);
        assert_eq!(cmd.z_index, -1);
        assert!(cmd.payload.is_empty());
    }

    #[test]
    fn parse_delete_all() {
        let data = b"a=d,d=a";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.action, Action::Delete);
        assert_eq!(cmd.delete, Some(DeleteTarget::All));
    }

    #[test]
    fn parse_delete_by_id() {
        let data = b"a=d,d=i,i=42";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.action, Action::Delete);
        assert_eq!(cmd.delete, Some(DeleteTarget::ById));
        assert_eq!(cmd.image_id, 42);
    }

    #[test]
    fn parse_chunked_more() {
        let data = b"a=T,f=100,m=1;chunk1data";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.action, Action::TransmitAndDisplay);
        assert!(cmd.more_chunks);
        assert_eq!(cmd.payload, b"chunk1data");
    }

    #[test]
    fn parse_chunked_last() {
        let data = b"m=0;lastchunk";
        let cmd = parse_command(data).unwrap();
        assert!(!cmd.more_chunks);
        assert_eq!(cmd.payload, b"lastchunk");
    }

    #[test]
    fn parse_compression_zlib() {
        let data = b"f=32,o=z,s=10,v=10;compresseddata";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.compression, Compression::Zlib);
    }

    #[test]
    fn parse_quiet_modes() {
        let data = b"a=T,q=1,f=100;data";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.quiet, Quiet::SuppressOk);

        let data = b"a=T,q=2,f=100;data";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.quiet, Quiet::SuppressAll);
    }

    #[test]
    fn parse_placement_keys() {
        let data = b"a=p,i=1,x=10,y=20,w=100,h=50,X=5,Y=3,C=1";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.src_x, 10);
        assert_eq!(cmd.src_y, 20);
        assert_eq!(cmd.src_w, 100);
        assert_eq!(cmd.src_h, 50);
        assert_eq!(cmd.offset_x, 5);
        assert_eq!(cmd.offset_y, 3);
        assert_eq!(cmd.cursor_movement, 1);
    }

    #[test]
    fn parse_empty_payload() {
        let data = b"a=q,i=1";
        let cmd = parse_command(data).unwrap();
        assert!(cmd.payload.is_empty());
    }

    #[test]
    fn parse_with_semicolon_no_payload() {
        let data = b"a=q,i=1;";
        let cmd = parse_command(data).unwrap();
        assert!(cmd.payload.is_empty());
    }

    #[test]
    fn parse_unknown_keys_ignored() {
        // Unknown keys are silently ignored.
        let data = b"a=T,Z=999,f=100;data";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.action, Action::TransmitAndDisplay);
        assert_eq!(cmd.format, Format::Png);
    }

    #[test]
    fn parse_animation_frame() {
        let data = b"a=f,i=1,r=2,z=40;framedata";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.action, Action::TransmitFrame);
        assert_eq!(cmd.image_id, 1);
        assert_eq!(cmd.rows, 2); // overloaded as edit_frame
        assert_eq!(cmd.z_index, 40); // overloaded as gap_ms
    }

    #[test]
    fn apc_buffer_kitty_sequence() {
        let mut buf = KittyApcBuffer::new();
        buf.start();
        
        // Simulate bytes: G a = T , f = 1 0 0 ; d a t a
        for &b in b"Ga=T,f=100;data" {
            buf.put(b);
        }
        
        let cmd = buf.finish().unwrap();
        assert_eq!(cmd.action, Action::TransmitAndDisplay);
        assert_eq!(cmd.format, Format::Png);
        assert_eq!(cmd.payload, b"data");
    }

    #[test]
    fn apc_buffer_non_kitty_sequence() {
        let mut buf = KittyApcBuffer::new();
        buf.start();
        
        // Not starting with 'G'.
        assert!(!buf.put(b'X'));
        assert!(buf.finish().is_none());
    }

    #[test]
    fn apc_buffer_reuse() {
        let mut buf = KittyApcBuffer::new();
        
        // First sequence.
        buf.start();
        for &b in b"Ga=q,i=1" {
            buf.put(b);
        }
        let cmd = buf.finish().unwrap();
        assert_eq!(cmd.action, Action::Query);
        
        // Second sequence (reuse buffer).
        buf.start();
        for &b in b"Ga=d,d=a" {
            buf.put(b);
        }
        let cmd = buf.finish().unwrap();
        assert_eq!(cmd.action, Action::Delete);
    }

    #[test]
    fn parse_relative_placement() {
        let data = b"a=p,i=1,P=10,Q=2,H=-5,V=3";
        let cmd = parse_command(data).unwrap();
        assert_eq!(cmd.parent_id, 10);
        assert_eq!(cmd.parent_placement_id, 2);
        assert_eq!(cmd.horizontal_offset, -5);
        assert_eq!(cmd.vertical_offset, 3);
    }
}