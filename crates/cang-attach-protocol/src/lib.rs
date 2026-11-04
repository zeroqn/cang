use anyhow::{Context, Result, anyhow, bail};
use std::io::{Read, Write};

pub mod terminal_trace;

pub const PROTOCOL_VERSION: u16 = 1;
pub const DEFAULT_ATTACH_PORT: u32 = 50_426;
pub const DETACH_PREFIX_BYTE: u8 = 0x1c; // Ctrl-\\
pub const DETACH_SUFFIX_BYTE: u8 = 0x1c; // Ctrl-\\

const CSI_U_DETACH_KEY_CODE: u32 = b'\\' as u32;
const CSI_U_CTRL_MODIFIER_BIT: u32 = 0b100;
const CSI_U_PRESS_EVENT: u32 = 1;
const MAX_CSI_U_SEQUENCE_LEN: usize = 64;
const MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024;
const TAG_HELLO: u8 = 1;
const TAG_ATTACH: u8 = 2;
const TAG_DATA: u8 = 3;
const TAG_RESIZE: u8 = 4;
const TAG_DETACH: u8 = 5;
const TAG_EXIT: u8 = 6;
const TAG_ERROR: u8 = 7;
const TAG_BUSY: u8 = 8;
const TAG_FORCE_ATTACH: u8 = 9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Hello { version: u16 },
    Attach,
    Data(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Detach,
    Exit { code: i32 },
    Error(String),
    Busy,
    // A busy-rejected client asks the guest to take the PTY over from the
    // client that currently holds it.
    ForceAttach,
}

impl Frame {
    fn tag(&self) -> u8 {
        match self {
            Self::Hello { .. } => TAG_HELLO,
            Self::Attach => TAG_ATTACH,
            Self::Data(_) => TAG_DATA,
            Self::Resize { .. } => TAG_RESIZE,
            Self::Detach => TAG_DETACH,
            Self::Exit { .. } => TAG_EXIT,
            Self::Error(_) => TAG_ERROR,
            Self::Busy => TAG_BUSY,
            Self::ForceAttach => TAG_FORCE_ATTACH,
        }
    }

    fn payload(&self) -> Vec<u8> {
        match self {
            Self::Hello { version } => version.to_be_bytes().to_vec(),
            Self::Attach | Self::Detach | Self::Busy | Self::ForceAttach => Vec::new(),
            Self::Data(data) => data.clone(),
            Self::Resize { rows, cols } => [rows.to_be_bytes(), cols.to_be_bytes()].concat(),
            Self::Exit { code } => code.to_be_bytes().to_vec(),
            Self::Error(message) => message.as_bytes().to_vec(),
        }
    }
}

pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> Result<()> {
    let payload = frame.payload();
    if payload.len() > MAX_PAYLOAD_LEN {
        bail!("cang attach protocol payload is too large");
    }
    writer.write_all(&[frame.tag()])?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame(reader: &mut impl Read) -> Result<Option<Frame>> {
    let mut header = [0u8; 5];
    match read_exact_or_clean_eof(reader, &mut header)
        .context("failed to read cang attach protocol frame header")?
    {
        ReadExactOutcome::Complete => {}
        ReadExactOutcome::CleanEof => return Ok(None),
    }
    let payload_len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        bail!("cang attach protocol frame payload exceeds maximum length");
    }
    let mut payload = vec![0u8; payload_len];
    reader
        .read_exact(&mut payload)
        .context("failed to read cang attach protocol frame payload")?;
    decode_frame(header[0], payload).map(Some)
}

enum ReadExactOutcome {
    Complete,
    CleanEof,
}

fn read_exact_or_clean_eof(reader: &mut impl Read, buf: &mut [u8]) -> Result<ReadExactOutcome> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(ReadExactOutcome::CleanEof),
            Ok(0) => bail!("truncated frame header"),
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(ReadExactOutcome::Complete)
}

fn decode_frame(tag: u8, payload: Vec<u8>) -> Result<Frame> {
    match tag {
        TAG_HELLO => {
            if payload.len() != 2 {
                bail!("invalid hello payload length");
            }
            Ok(Frame::Hello {
                version: u16::from_be_bytes([payload[0], payload[1]]),
            })
        }
        TAG_ATTACH => empty_payload(payload, Frame::Attach),
        TAG_DATA => Ok(Frame::Data(payload)),
        TAG_RESIZE => {
            if payload.len() != 4 {
                bail!("invalid resize payload length");
            }
            Ok(Frame::Resize {
                rows: u16::from_be_bytes([payload[0], payload[1]]),
                cols: u16::from_be_bytes([payload[2], payload[3]]),
            })
        }
        TAG_DETACH => empty_payload(payload, Frame::Detach),
        TAG_EXIT => {
            if payload.len() != 4 {
                bail!("invalid exit payload length");
            }
            Ok(Frame::Exit {
                code: i32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
            })
        }
        TAG_ERROR => Ok(Frame::Error(
            String::from_utf8(payload).map_err(|_| anyhow!("error payload is not utf-8"))?,
        )),
        TAG_BUSY => empty_payload(payload, Frame::Busy),
        TAG_FORCE_ATTACH => empty_payload(payload, Frame::ForceAttach),
        _ => bail!("unknown cang attach protocol frame tag {tag}"),
    }
}

fn empty_payload(payload: Vec<u8>, frame: Frame) -> Result<Frame> {
    if !payload.is_empty() {
        bail!("frame must have an empty payload");
    }
    Ok(frame)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DetachFilter {
    detached: bool,
    pending_detach_key: Option<Vec<u8>>,
    pending_csi_sequence: Vec<u8>,
}

impl DetachFilter {
    pub fn push(&mut self, input: &[u8], output: &mut Vec<u8>) -> bool {
        for &byte in input {
            match self.parse_input_byte(byte) {
                ParsedInput::DetachKey(bytes) => {
                    if self.pending_detach_key.take().is_some() {
                        self.detached = true;
                        return true;
                    }
                    self.pending_detach_key = Some(bytes);
                }
                ParsedInput::Bytes(bytes) => {
                    self.flush_pending_detach_key(output);
                    output.extend(bytes);
                }
                ParsedInput::Byte(byte) => {
                    self.flush_pending_detach_key(output);
                    output.push(byte);
                }
                ParsedInput::Pending => {}
            }
        }
        false
    }

    pub fn flush_pending(&mut self, output: &mut Vec<u8>) {
        self.flush_pending_detach_key(output);
        self.flush_incomplete_escape_sequence(output);
    }

    pub fn flush_incomplete_escape_sequence(&mut self, output: &mut Vec<u8>) {
        output.extend(std::mem::take(&mut self.pending_csi_sequence));
    }

    pub fn detached(&self) -> bool {
        self.detached
    }

    fn parse_input_byte(&mut self, byte: u8) -> ParsedInput {
        if !self.pending_csi_sequence.is_empty() {
            return self.parse_pending_csi_byte(byte);
        }

        match byte {
            DETACH_PREFIX_BYTE => ParsedInput::DetachKey(vec![byte]),
            b'\x1b' => {
                self.pending_csi_sequence.push(byte);
                ParsedInput::Pending
            }
            _ => ParsedInput::Byte(byte),
        }
    }

    fn parse_pending_csi_byte(&mut self, byte: u8) -> ParsedInput {
        self.pending_csi_sequence.push(byte);

        if !is_potential_csi_u_sequence(&self.pending_csi_sequence)
            || self.pending_csi_sequence.len() > MAX_CSI_U_SEQUENCE_LEN
        {
            return ParsedInput::Bytes(std::mem::take(&mut self.pending_csi_sequence));
        }

        if byte != b'u' {
            return ParsedInput::Pending;
        }

        let bytes = std::mem::take(&mut self.pending_csi_sequence);
        if is_csi_u_detach_key(&bytes) {
            ParsedInput::DetachKey(bytes)
        } else {
            ParsedInput::Bytes(bytes)
        }
    }

    fn flush_pending_detach_key(&mut self, output: &mut Vec<u8>) {
        if let Some(bytes) = self.pending_detach_key.take() {
            output.extend(bytes);
        }
    }
}

enum ParsedInput {
    DetachKey(Vec<u8>),
    Bytes(Vec<u8>),
    Byte(u8),
    Pending,
}

fn is_potential_csi_u_sequence(bytes: &[u8]) -> bool {
    match bytes {
        [b'\x1b'] => true,
        [b'\x1b', b'['] => true,
        [b'\x1b', b'[', rest @ ..] => rest
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b';' | b':' | b'u')),
        _ => false,
    }
}

fn is_csi_u_detach_key(bytes: &[u8]) -> bool {
    let body = match bytes {
        [b'\x1b', b'[', body @ .., b'u'] => body,
        _ => return false,
    };
    let Ok(body) = std::str::from_utf8(body) else {
        return false;
    };
    let Some(event) = parse_csi_u_event(body) else {
        return false;
    };
    event.key_code == CSI_U_DETACH_KEY_CODE
        && event.modifiers & CSI_U_CTRL_MODIFIER_BIT != 0
        && event.event_type == CSI_U_PRESS_EVENT
}

fn parse_csi_u_event(body: &str) -> Option<CsiUEvent> {
    let mut fields = body.split(';');
    let key_field = fields.next()?;
    let key_code = key_field.split(':').next()?.parse().ok()?;
    let (encoded_modifiers, event_type) = parse_csi_u_modifier_event_field(fields.next())?;

    Some(CsiUEvent {
        key_code,
        modifiers: encoded_modifiers.checked_sub(1)?,
        event_type,
    })
}

fn parse_csi_u_modifier_event_field(field: Option<&str>) -> Option<(u32, u32)> {
    let Some(field) = field else {
        return Some((1, CSI_U_PRESS_EVENT));
    };
    let mut parts = field.split(':');
    let modifiers = match parts.next()? {
        "" => 1,
        value => value.parse().ok()?,
    };
    let event_type = match parts.next() {
        Some("") | None => CSI_U_PRESS_EVENT,
        Some(value) => value.parse().ok()?,
    };
    Some((modifiers, event_type))
}

struct CsiUEvent {
    key_code: u32,
    modifiers: u32,
    event_type: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_round_trip() {
        let frames = vec![
            Frame::Hello { version: 1 },
            Frame::Attach,
            Frame::Data(vec![0, 1, 2, 255]),
            Frame::Resize { rows: 24, cols: 80 },
            Frame::Detach,
            Frame::Exit { code: 42 },
            Frame::Error("nope".to_owned()),
            Frame::Busy,
            Frame::ForceAttach,
        ];
        let mut bytes = Vec::new();
        for frame in &frames {
            write_frame(&mut bytes, frame).expect("write frame");
        }
        let mut cursor = Cursor::new(bytes);
        for expected in frames {
            assert_eq!(read_frame(&mut cursor).expect("read frame"), Some(expected));
        }
        assert_eq!(read_frame(&mut cursor).expect("eof"), None);
    }

    #[test]
    fn incomplete_payload_fails_deterministically() {
        let mut cursor = Cursor::new(vec![TAG_DATA, 0, 0, 0, 2, 1]);
        let err = read_frame(&mut cursor).expect_err("short payload should fail");
        assert!(format!("{err:#}").contains("payload"));
    }

    #[test]
    fn partial_header_fails_instead_of_detaching() {
        let mut cursor = Cursor::new(vec![TAG_DATA, 0, 0]);
        let err = read_frame(&mut cursor).expect_err("partial header should fail");
        assert!(format!("{err:#}").contains("truncated frame header"));
    }

    #[test]
    fn detach_filter_consumes_ctrl_backslash_twice() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(filter.push(b"ab\x1c\x1ccd", &mut output));
        assert_eq!(output, b"ab");
        assert!(filter.detached());
    }

    #[test]
    fn detach_filter_matches_sequence_across_reads() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1c", &mut output));
        assert_eq!(output, b"ab");

        assert!(filter.push(b"\x1ccd", &mut output));

        assert_eq!(output, b"ab");
        assert!(filter.detached());
    }

    #[test]
    fn detach_filter_consumes_csi_u_ctrl_backslash_twice() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(filter.push(b"ab\x1b[92;133u\x1b[92;133ucd", &mut output));
        assert_eq!(output, b"ab");
        assert!(filter.detached());
    }

    #[test]
    fn detach_filter_consumes_csi_u_ctrl_backslash_ctrl_only_modifier() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(filter.push(b"ab\x1b[92;5u\x1b[92;5ucd", &mut output));
        assert_eq!(output, b"ab");
        assert!(filter.detached());
    }

    #[test]
    fn detach_filter_matches_mixed_raw_and_csi_u_ctrl_backslash() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(filter.push(b"ab\x1c\x1b[92;133ucd", &mut output));
        assert_eq!(output, b"ab");
        assert!(filter.detached());

        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(filter.push(b"ab\x1b[92;133u\x1ccd", &mut output));
        assert_eq!(output, b"ab");
        assert!(filter.detached());
    }

    #[test]
    fn detach_filter_matches_csi_u_sequence_across_reads() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1b[92;", &mut output));
        assert_eq!(output, b"ab");
        assert!(!filter.push(b"133u", &mut output));
        assert_eq!(output, b"ab");

        assert!(filter.push(b"\x1b[92;133ucd", &mut output));

        assert_eq!(output, b"ab");
        assert!(filter.detached());
    }

    #[test]
    fn detach_filter_forwards_csi_u_without_ctrl() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1b[92;1u\x1b[92;1ucd", &mut output));
        assert_eq!(output, b"ab\x1b[92;1u\x1b[92;1ucd");
        assert!(!filter.detached());
    }

    #[test]
    fn detach_filter_forwards_non_backslash_csi_u_with_ctrl() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1b[103;5u\x1b[103;5ucd", &mut output));
        assert_eq!(output, b"ab\x1b[103;5u\x1b[103;5ucd");
        assert!(!filter.detached());
    }

    #[test]
    fn detach_filter_does_not_detach_on_csi_u_release_event() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1b[92;5:3u\x1b[92;5:3ucd", &mut output));
        assert_eq!(output, b"ab\x1b[92;5:3u\x1b[92;5:3ucd");
        assert!(!filter.detached());
    }

    #[test]
    fn detach_filter_flushes_partial_csi_u_sequence() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1b[92;", &mut output));
        assert_eq!(output, b"ab");

        filter.flush_pending(&mut output);

        assert_eq!(output, b"ab\x1b[92;");
        assert!(!filter.detached());
    }

    #[test]
    fn detach_filter_timeout_flush_preserves_pending_ctrl_backslash() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1c", &mut output));
        assert_eq!(output, b"ab");

        filter.flush_incomplete_escape_sequence(&mut output);

        assert_eq!(output, b"ab");
        assert!(filter.push(b"\x1ccd", &mut output));
        assert_eq!(output, b"ab");
        assert!(filter.detached());
    }

    #[test]
    fn detach_filter_timeout_flushes_standalone_escape() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1b", &mut output));
        assert_eq!(output, b"ab");

        filter.flush_incomplete_escape_sequence(&mut output);

        assert_eq!(output, b"ab\x1b");
        assert!(!filter.detached());
    }

    #[test]
    fn detach_filter_forwards_ctrl_backslash_when_sequence_does_not_match() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1cx", &mut output));
        assert_eq!(output, b"ab\x1cx");
        assert!(!filter.detached());
    }

    #[test]
    fn detach_filter_flushes_standalone_ctrl_backslash() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1c", &mut output));
        assert_eq!(output, b"ab");

        filter.flush_pending(&mut output);

        assert_eq!(output, b"ab\x1c");
        assert!(!filter.detached());
    }

    #[test]
    fn ctrl_d_is_regular_input() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x04cd", &mut output));
        assert_eq!(output, b"ab\x04cd");
        assert!(!filter.detached());
    }

    #[test]
    fn ctrl_g_is_regular_input() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x07\x07cd", &mut output));
        assert_eq!(output, b"ab\x07\x07cd");
        assert!(!filter.detached());
    }

    #[test]
    fn csi_u_ctrl_g_is_regular_input() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x1b[103;133u\x1b[103;133ucd", &mut output));
        assert_eq!(output, b"ab\x1b[103;133u\x1b[103;133ucd");
        assert!(!filter.detached());
    }

    #[test]
    fn ctrl_p_ctrl_q_is_regular_input() {
        let mut filter = DetachFilter::default();
        let mut output = Vec::new();
        assert!(!filter.push(b"ab\x10\x11cd", &mut output));
        assert_eq!(output, b"ab\x10\x11cd");
        assert!(!filter.detached());
    }
}
