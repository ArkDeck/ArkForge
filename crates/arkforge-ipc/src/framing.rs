//! Frame layer.
//!
//! architecture.md 15.2: frame size limit, request id, stream sequence. A frame
//! is a 4-byte big-endian length followed by that many bytes of message. The
//! length is checked against [`wire::MAX_FRAME_BYTES`] *before* allocation, so
//! a peer cannot make the daemon reserve memory by claiming a large frame.

use crate::wire::{self, WireError};
use std::io::{self, Read, Write};

/// Reads one length-prefixed frame.
///
/// Returns `Ok(None)` at a clean end of stream — the peer closing between
/// frames is not an error.
pub fn read_frame<R: Read>(source: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = [0u8; 4];
    let mut filled = 0usize;
    while filled < header.len() {
        let count = source.read(&mut header[filled..])?;
        if count == 0 {
            if filled == 0 {
                return Ok(None);
            }
            return Err(FrameError::Truncated);
        }
        filled += count;
    }
    let length = u32::from_be_bytes(header) as usize;
    if length > wire::MAX_FRAME_BYTES {
        return Err(FrameError::Wire(WireError::FrameTooLarge(length)));
    }
    let mut body = vec![0u8; length];
    let mut filled = 0usize;
    while filled < length {
        let count = source.read(&mut body[filled..])?;
        if count == 0 {
            return Err(FrameError::Truncated);
        }
        filled += count;
    }
    Ok(Some(body))
}

/// Writes one length-prefixed frame.
pub fn write_frame<W: Write>(sink: &mut W, body: &[u8]) -> Result<(), FrameError> {
    if body.len() > wire::MAX_FRAME_BYTES {
        return Err(FrameError::Wire(WireError::FrameTooLarge(body.len())));
    }
    sink.write_all(&(body.len() as u32).to_be_bytes())?;
    sink.write_all(body)?;
    sink.flush()?;
    Ok(())
}

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Truncated,
    Wire(WireError),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(error) => write!(f, "frame I/O failed: {error}"),
            FrameError::Truncated => f.write_str("connection closed mid-frame"),
            FrameError::Wire(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        FrameError::Io(error)
    }
}

impl From<WireError> for FrameError {
    fn from(error: WireError) -> Self {
        FrameError::Wire(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, b"first").unwrap();
        write_frame(&mut buffer, b"second frame").unwrap();

        let mut cursor = buffer.as_slice();
        assert_eq!(read_frame(&mut cursor).unwrap().unwrap(), b"first");
        assert_eq!(read_frame(&mut cursor).unwrap().unwrap(), b"second frame");
        assert_eq!(read_frame(&mut cursor).unwrap(), None);
    }

    #[test]
    fn an_oversized_declared_length_is_refused_before_allocation() {
        // A 4 GiB claim in a 4-byte message.
        let malicious = [0xffu8, 0xff, 0xff, 0xff];
        let mut cursor = malicious.as_slice();
        let error = read_frame(&mut cursor).unwrap_err();
        assert!(
            matches!(error, FrameError::Wire(WireError::FrameTooLarge(_))),
            "{error}"
        );
    }

    #[test]
    fn a_truncated_body_is_an_error_not_a_short_frame() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, b"complete payload").unwrap();
        buffer.truncate(buffer.len() - 4);
        let mut cursor = buffer.as_slice();
        assert!(matches!(
            read_frame(&mut cursor).unwrap_err(),
            FrameError::Truncated
        ));
    }

    #[test]
    fn a_truncated_header_is_an_error() {
        let partial = [0u8, 0u8];
        let mut cursor = partial.as_slice();
        assert!(matches!(
            read_frame(&mut cursor).unwrap_err(),
            FrameError::Truncated
        ));
    }

    #[test]
    fn an_empty_frame_is_legal() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, b"").unwrap();
        let mut cursor = buffer.as_slice();
        assert_eq!(read_frame(&mut cursor).unwrap().unwrap(), Vec::<u8>::new());
    }
}
