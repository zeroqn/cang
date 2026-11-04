use anyhow::{Context, Result, anyhow, bail};
use std::io::{Read, Write};

pub const PROTOCOL_VERSION: u16 = 3;
pub const DEFAULT_EXEC_PORT: u32 = 50_428;

const MAX_PAYLOAD_LEN: usize = 1024 * 1024;
const MAX_ARGC: usize = 4096;
const MAX_ARG_LEN: usize = 256 * 1024;

const TAG_HELLO: u8 = 1;
const TAG_READY: u8 = 2;
const TAG_START: u8 = 3;
const TAG_STDIN: u8 = 4;
const TAG_STDIN_EOF: u8 = 5;
const TAG_SIGNAL: u8 = 6;
const TAG_STDOUT: u8 = 7;
const TAG_STDERR: u8 = 8;
const TAG_EXIT: u8 = 9;
const TAG_ERROR: u8 = 10;
const TAG_RECLAIM_MEMORY: u8 = 11;
const TAG_RECLAIMED_MEMORY: u8 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaypipeAction {
    Disabled,
    Reuse,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Hello {
        version: u16,
    },
    Ready,
    Start {
        argv: Vec<String>,
        waypipe: WaypipeAction,
    },
    Stdin(Vec<u8>),
    StdinEof,
    Signal {
        signal: i32,
    },
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit {
        code: i32,
    },
    Error(String),
    /// Ask the guest to release its reclaimable caches to the host kernel.
    ///
    /// The guest answers with [`Frame::ReclaimedMemory`], or with
    /// [`Frame::Error`] when it could not free anything.
    ReclaimMemory,
    ReclaimedMemory {
        /// Guest `MemFree` growth, in KiB, that the release produced.
        freed_kib: u64,
    },
}

impl Frame {
    fn tag(&self) -> u8 {
        match self {
            Self::Hello { .. } => TAG_HELLO,
            Self::Ready => TAG_READY,
            Self::Start { .. } => TAG_START,
            Self::Stdin(_) => TAG_STDIN,
            Self::StdinEof => TAG_STDIN_EOF,
            Self::Signal { .. } => TAG_SIGNAL,
            Self::Stdout(_) => TAG_STDOUT,
            Self::Stderr(_) => TAG_STDERR,
            Self::Exit { .. } => TAG_EXIT,
            Self::Error(_) => TAG_ERROR,
            Self::ReclaimMemory => TAG_RECLAIM_MEMORY,
            Self::ReclaimedMemory { .. } => TAG_RECLAIMED_MEMORY,
        }
    }

    fn payload(&self) -> Result<Vec<u8>> {
        match self {
            Self::Hello { version } => Ok(version.to_be_bytes().to_vec()),
            Self::Ready | Self::StdinEof => Ok(Vec::new()),
            Self::Start { argv, waypipe } => encode_start(argv, *waypipe),
            Self::Stdin(data) | Self::Stdout(data) | Self::Stderr(data) => Ok(data.clone()),
            Self::Signal { signal } | Self::Exit { code: signal } => {
                Ok(signal.to_be_bytes().to_vec())
            }
            Self::Error(message) => Ok(message.as_bytes().to_vec()),
            Self::ReclaimMemory => Ok(Vec::new()),
            Self::ReclaimedMemory { freed_kib } => Ok(freed_kib.to_be_bytes().to_vec()),
        }
    }
}

pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> Result<()> {
    let payload = frame.payload()?;
    if payload.len() > MAX_PAYLOAD_LEN {
        bail!("cang exec protocol payload is too large");
    }
    writer
        .write_all(&[frame.tag()])
        .context("failed to write cang exec protocol frame tag")?;
    writer
        .write_all(&(payload.len() as u32).to_be_bytes())
        .context("failed to write cang exec protocol frame length")?;
    writer
        .write_all(&payload)
        .context("failed to write cang exec protocol frame payload")?;
    writer
        .flush()
        .context("failed to flush cang exec protocol frame")
}

pub fn read_frame(reader: &mut impl Read) -> Result<Option<Frame>> {
    let mut header = [0u8; 5];
    match read_exact_or_eof(reader, &mut header)
        .context("failed to read cang exec protocol frame header")?
    {
        ReadExactOutcome::Complete => {}
        ReadExactOutcome::CleanEof => return Ok(None),
    }
    let payload_len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        bail!("cang exec protocol frame payload exceeds maximum length");
    }
    let mut payload = vec![0; payload_len];
    reader
        .read_exact(&mut payload)
        .context("failed to read cang exec protocol frame payload")?;
    decode_frame(header[0], payload).map(Some)
}

fn decode_frame(tag: u8, payload: Vec<u8>) -> Result<Frame> {
    match tag {
        TAG_HELLO => Ok(Frame::Hello {
            version: decode_u16(&payload, "hello")?,
        }),
        TAG_READY => {
            require_empty(&payload, "ready")?;
            Ok(Frame::Ready)
        }
        TAG_START => {
            let (argv, waypipe) = decode_start(&payload)?;
            Ok(Frame::Start { argv, waypipe })
        }
        TAG_STDIN => Ok(Frame::Stdin(payload)),
        TAG_STDIN_EOF => {
            require_empty(&payload, "stdin EOF")?;
            Ok(Frame::StdinEof)
        }
        TAG_SIGNAL => Ok(Frame::Signal {
            signal: decode_i32(&payload, "signal")?,
        }),
        TAG_STDOUT => Ok(Frame::Stdout(payload)),
        TAG_STDERR => Ok(Frame::Stderr(payload)),
        TAG_EXIT => Ok(Frame::Exit {
            code: decode_i32(&payload, "exit")?,
        }),
        TAG_ERROR => Ok(Frame::Error(
            String::from_utf8(payload).map_err(|_| anyhow!("error payload is not utf-8"))?,
        )),
        TAG_RECLAIM_MEMORY => {
            require_empty(&payload, "reclaim memory")?;
            Ok(Frame::ReclaimMemory)
        }
        TAG_RECLAIMED_MEMORY => Ok(Frame::ReclaimedMemory {
            freed_kib: decode_u64(&payload, "reclaimed memory")?,
        }),
        _ => bail!("unknown cang exec protocol frame tag {tag}"),
    }
}

fn encode_start(argv: &[String], waypipe: WaypipeAction) -> Result<Vec<u8>> {
    let mut payload = vec![match waypipe {
        WaypipeAction::Disabled => 0,
        WaypipeAction::Reuse => 1,
        WaypipeAction::Replace => 2,
    }];
    payload.extend(encode_argv(argv)?);
    Ok(payload)
}

fn decode_start(payload: &[u8]) -> Result<(Vec<String>, WaypipeAction)> {
    let (&action, argv) = payload
        .split_first()
        .ok_or_else(|| anyhow!("cang exec start payload is empty"))?;
    let waypipe = match action {
        0 => WaypipeAction::Disabled,
        1 => WaypipeAction::Reuse,
        2 => WaypipeAction::Replace,
        _ => bail!("cang exec start Waypipe action is invalid"),
    };
    Ok((decode_argv(argv)?, waypipe))
}

fn encode_argv(argv: &[String]) -> Result<Vec<u8>> {
    if argv.is_empty() {
        bail!("cang exec start argv must not be empty");
    }
    if argv.len() > MAX_ARGC {
        bail!("cang exec start argv has too many arguments");
    }
    let mut payload = Vec::new();
    payload.extend((argv.len() as u32).to_be_bytes());
    for arg in argv {
        let bytes = arg.as_bytes();
        if bytes.len() > MAX_ARG_LEN {
            bail!("cang exec argument is too large");
        }
        payload.extend((bytes.len() as u32).to_be_bytes());
        payload.extend(bytes);
    }
    Ok(payload)
}

fn decode_argv(payload: &[u8]) -> Result<Vec<String>> {
    let mut offset = 0;
    let argc = read_u32(payload, &mut offset, "argv count")? as usize;
    if argc == 0 {
        bail!("cang exec start argv must not be empty");
    }
    if argc > MAX_ARGC {
        bail!("cang exec start argv has too many arguments");
    }
    let mut argv = Vec::with_capacity(argc);
    for _ in 0..argc {
        let len = read_u32(payload, &mut offset, "argument length")? as usize;
        if len > MAX_ARG_LEN {
            bail!("cang exec argument is too large");
        }
        let end = offset
            .checked_add(len)
            .filter(|end| *end <= payload.len())
            .ok_or_else(|| anyhow!("cang exec start argv is truncated"))?;
        let arg = String::from_utf8(payload[offset..end].to_vec())
            .map_err(|_| anyhow!("cang exec argument is not utf-8"))?;
        argv.push(arg);
        offset = end;
    }
    if offset != payload.len() {
        bail!("cang exec start argv has trailing bytes");
    }
    Ok(argv)
}

fn read_u32(payload: &[u8], offset: &mut usize, field: &str) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .filter(|end| *end <= payload.len())
        .ok_or_else(|| anyhow!("cang exec {field} is truncated"))?;
    let value = u32::from_be_bytes(payload[*offset..end].try_into().unwrap());
    *offset = end;
    Ok(value)
}

fn decode_u16(payload: &[u8], frame: &str) -> Result<u16> {
    let bytes: [u8; 2] = payload
        .try_into()
        .map_err(|_| anyhow!("cang exec {frame} payload must contain a u16"))?;
    Ok(u16::from_be_bytes(bytes))
}

fn decode_u64(payload: &[u8], frame: &str) -> Result<u64> {
    let bytes: [u8; 8] = payload
        .try_into()
        .map_err(|_| anyhow!("cang exec {frame} payload must contain a u64"))?;
    Ok(u64::from_be_bytes(bytes))
}

fn decode_i32(payload: &[u8], frame: &str) -> Result<i32> {
    let bytes: [u8; 4] = payload
        .try_into()
        .map_err(|_| anyhow!("cang exec {frame} payload must contain an i32"))?;
    Ok(i32::from_be_bytes(bytes))
}

fn require_empty(payload: &[u8], frame: &str) -> Result<()> {
    if !payload.is_empty() {
        bail!("cang exec {frame} payload must be empty");
    }
    Ok(())
}

enum ReadExactOutcome {
    Complete,
    CleanEof,
}

fn read_exact_or_eof(
    reader: &mut impl Read,
    buffer: &mut [u8],
) -> std::io::Result<ReadExactOutcome> {
    let mut offset = 0;
    while offset < buffer.len() {
        match reader.read(&mut buffer[offset..])? {
            0 if offset == 0 => return Ok(ReadExactOutcome::CleanEof),
            0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "unexpected EOF",
                ));
            }
            read => offset += read,
        }
    }
    Ok(ReadExactOutcome::Complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn round_trip(frame: Frame) {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).unwrap();
        assert_eq!(read_frame(&mut Cursor::new(bytes)).unwrap(), Some(frame));
    }

    #[test]
    fn frames_round_trip() {
        for frame in [
            Frame::Hello { version: 1 },
            Frame::Ready,
            Frame::Start {
                argv: vec!["sh".into(), "-c".into(), "echo hello".into()],
                waypipe: WaypipeAction::Reuse,
            },
            Frame::Stdin(vec![0, 1, 2]),
            Frame::StdinEof,
            Frame::Signal { signal: 2 },
            Frame::Stdout(b"out".to_vec()),
            Frame::Stderr(b"err".to_vec()),
            Frame::Exit { code: 23 },
            Frame::Error("failed".into()),
            Frame::ReclaimMemory,
            Frame::ReclaimedMemory { freed_kib: 0 },
            Frame::ReclaimedMemory {
                freed_kib: u64::MAX - 7,
            },
        ] {
            round_trip(frame);
        }
    }

    #[test]
    fn argv_preserves_boundaries() {
        round_trip(Frame::Start {
            argv: vec!["printf".into(), "a b".into(), "'quoted'".into(), "".into()],
            waypipe: WaypipeAction::Replace,
        });
    }

    #[test]
    fn clean_eof_returns_none() {
        assert_eq!(
            read_frame(&mut Cursor::new(Vec::<u8>::new())).unwrap(),
            None
        );
    }

    #[test]
    fn empty_argv_is_rejected() {
        let err = write_frame(
            &mut Vec::new(),
            &Frame::Start {
                argv: Vec::new(),
                waypipe: WaypipeAction::Disabled,
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("must not be empty"));
    }

    #[test]
    fn reclaimed_memory_payload_must_be_a_u64() {
        let bytes = vec![TAG_RECLAIMED_MEMORY, 0, 0, 0, 4, 0, 0, 0, 1];
        let err = read_frame(&mut Cursor::new(bytes)).unwrap_err();
        assert!(format!("{err:#}").contains("u64"));
    }

    #[test]
    fn reclaim_memory_payload_must_be_empty() {
        let bytes = vec![TAG_RECLAIM_MEMORY, 0, 0, 0, 1, 0];
        let err = read_frame(&mut Cursor::new(bytes)).unwrap_err();
        assert!(format!("{err:#}").contains("must be empty"));
    }

    #[test]
    fn malformed_payload_is_rejected() {
        let bytes = vec![TAG_START, 0, 0, 0, 4, 0, 0, 0, 1];
        let err = read_frame(&mut Cursor::new(bytes)).unwrap_err();
        assert!(format!("{err:#}").contains("truncated"));
    }
}
