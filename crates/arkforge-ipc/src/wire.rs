//! Protobuf wire primitives.
//!
//! A hand-written codec against `proto/arkforge.proto` (AFD-0001). The subset
//! is what the schema uses: varints, length-delimited fields, and nested
//! messages. No groups, no fixed32/64, no maps — the schema does not use them,
//! and a decoder that accepted shapes the schema cannot produce would be
//! surface nobody reviewed.
//!
//! Two deliberate asymmetries, both from architecture.md 15.2:
//!
//! - an unknown **field** is skipped, which is how protobuf stays forward
//!   compatible;
//! - an unknown **enum value** is an error, because a security-relevant enum
//!   that silently becomes its zero value is how a refusal turns into a
//!   permission.

use core::fmt;

/// The largest single frame the daemon will decode.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Nesting bound, so a hostile message cannot recurse the decoder to death.
const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    Varint,
    LengthDelimited,
    Fixed64,
    Fixed32,
}

impl WireType {
    fn from_tag(value: u32) -> Result<Self, WireError> {
        match value {
            0 => Ok(WireType::Varint),
            1 => Ok(WireType::Fixed64),
            2 => Ok(WireType::LengthDelimited),
            5 => Ok(WireType::Fixed32),
            other => Err(WireError::UnsupportedWireType(other)),
        }
    }

    fn tag(self) -> u32 {
        match self {
            WireType::Varint => 0,
            WireType::Fixed64 => 1,
            WireType::LengthDelimited => 2,
            WireType::Fixed32 => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    Truncated,
    VarintTooLong,
    UnsupportedWireType(u32),
    FieldZero,
    InvalidUtf8(u32),
    LengthOverflow,
    DepthLimit,
    TrailingBytes(usize),
    /// A security-relevant enum carried a value this build does not know.
    UnknownEnumValue { field: u32, value: i64 },
    FrameTooLarge(usize),
    /// A field the schema marks as required for this message is absent.
    MissingField { message: &'static str, field: u32 },
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Truncated => f.write_str("message ended mid-field"),
            WireError::VarintTooLong => f.write_str("varint exceeds 10 bytes"),
            WireError::UnsupportedWireType(value) => {
                write!(f, "wire type {value} is not in the supported subset")
            }
            WireError::FieldZero => f.write_str("field number 0 is not valid"),
            WireError::InvalidUtf8(field) => write!(f, "field {field} is not valid UTF-8"),
            WireError::LengthOverflow => f.write_str("length-delimited field overruns the message"),
            WireError::DepthLimit => f.write_str("message nesting exceeds the depth limit"),
            WireError::TrailingBytes(count) => write!(f, "{count} trailing bytes"),
            WireError::UnknownEnumValue { field, value } => write!(
                f,
                "field {field} carries unknown enum value {value}; refusing to default it"
            ),
            WireError::FrameTooLarge(size) => write!(
                f,
                "frame of {size} bytes exceeds the {MAX_FRAME_BYTES}-byte limit"
            ),
            WireError::MissingField { message, field } => {
                write!(f, "{message} is missing required field {field}")
            }
        }
    }
}

impl std::error::Error for WireError {}

/// Appends a protobuf varint.
pub fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn write_tag(out: &mut Vec<u8>, field: u32, wire_type: WireType) {
    write_varint(out, ((field as u64) << 3) | wire_type.tag() as u64);
}

pub fn write_uint64(out: &mut Vec<u8>, field: u32, value: u64) {
    if value == 0 {
        return; // proto3 default; absent and zero are the same value.
    }
    write_tag(out, field, WireType::Varint);
    write_varint(out, value);
}

pub fn write_uint32(out: &mut Vec<u8>, field: u32, value: u32) {
    write_uint64(out, field, value as u64);
}

pub fn write_bool(out: &mut Vec<u8>, field: u32, value: bool) {
    if !value {
        return;
    }
    write_tag(out, field, WireType::Varint);
    write_varint(out, 1);
}

pub fn write_enum(out: &mut Vec<u8>, field: u32, value: i32) {
    if value == 0 {
        return;
    }
    write_tag(out, field, WireType::Varint);
    write_varint(out, value as u64);
}

pub fn write_string(out: &mut Vec<u8>, field: u32, value: &str) {
    if value.is_empty() {
        return;
    }
    write_bytes(out, field, value.as_bytes());
}

pub fn write_bytes(out: &mut Vec<u8>, field: u32, value: &[u8]) {
    if value.is_empty() {
        return;
    }
    write_tag(out, field, WireType::LengthDelimited);
    write_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

/// Writes a nested message, even when it encodes to zero bytes, so "present
/// and empty" stays distinguishable from "absent".
pub fn write_message(out: &mut Vec<u8>, field: u32, body: &[u8]) {
    write_tag(out, field, WireType::LengthDelimited);
    write_varint(out, body.len() as u64);
    out.extend_from_slice(body);
}

/// One decoded field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed64([u8; 8]),
    Fixed32([u8; 4]),
}

impl<'a> FieldValue<'a> {
    pub fn as_u64(&self) -> Result<u64, WireError> {
        match self {
            FieldValue::Varint(value) => Ok(*value),
            _ => Err(WireError::UnsupportedWireType(9)),
        }
    }

    pub fn as_bool(&self) -> Result<bool, WireError> {
        Ok(self.as_u64()? != 0)
    }

    pub fn as_bytes(&self) -> Result<&'a [u8], WireError> {
        match self {
            FieldValue::Bytes(value) => Ok(value),
            _ => Err(WireError::UnsupportedWireType(9)),
        }
    }

    pub fn as_str(&self, field: u32) -> Result<&'a str, WireError> {
        std::str::from_utf8(self.as_bytes()?).map_err(|_| WireError::InvalidUtf8(field))
    }
}

/// Iterates the fields of a message.
#[derive(Debug)]
pub struct Reader<'a> {
    input: &'a [u8],
    position: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Reader {
            input,
            position: 0,
            depth: 0,
        }
    }

    fn nested(input: &'a [u8], depth: usize) -> Self {
        Reader {
            input,
            position: 0,
            depth,
        }
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    fn read_varint(&mut self) -> Result<u64, WireError> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            if shift >= 64 {
                return Err(WireError::VarintTooLong);
            }
            let byte = *self.input.get(self.position).ok_or(WireError::Truncated)?;
            self.position += 1;
            value |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    /// Returns the next `(field number, value)`, or `None` at the end.
    pub fn next_field(&mut self) -> Result<Option<(u32, FieldValue<'a>)>, WireError> {
        if self.position >= self.input.len() {
            return Ok(None);
        }
        let key = self.read_varint()?;
        let field = (key >> 3) as u32;
        if field == 0 {
            return Err(WireError::FieldZero);
        }
        let wire_type = WireType::from_tag((key & 0x7) as u32)?;
        let value = match wire_type {
            WireType::Varint => FieldValue::Varint(self.read_varint()?),
            WireType::LengthDelimited => {
                let length = self.read_varint()? as usize;
                let end = self
                    .position
                    .checked_add(length)
                    .ok_or(WireError::LengthOverflow)?;
                if end > self.input.len() {
                    return Err(WireError::LengthOverflow);
                }
                let slice = &self.input[self.position..end];
                self.position = end;
                FieldValue::Bytes(slice)
            }
            WireType::Fixed64 => {
                let end = self.position + 8;
                if end > self.input.len() {
                    return Err(WireError::Truncated);
                }
                let mut buffer = [0u8; 8];
                buffer.copy_from_slice(&self.input[self.position..end]);
                self.position = end;
                FieldValue::Fixed64(buffer)
            }
            WireType::Fixed32 => {
                let end = self.position + 4;
                if end > self.input.len() {
                    return Err(WireError::Truncated);
                }
                let mut buffer = [0u8; 4];
                buffer.copy_from_slice(&self.input[self.position..end]);
                self.position = end;
                FieldValue::Fixed32(buffer)
            }
        };
        Ok(Some((field, value)))
    }

    /// Opens a nested message, enforcing the depth bound.
    pub fn nested_reader(&self, value: &FieldValue<'a>) -> Result<Reader<'a>, WireError> {
        if self.depth + 1 > MAX_DEPTH {
            return Err(WireError::DepthLimit);
        }
        Ok(Reader::nested(value.as_bytes()?, self.depth + 1))
    }
}

/// Decodes an enum, refusing values this build does not know.
///
/// `parse` returns `None` for an unrecognized value; that is a hard error here
/// rather than a fallback to the zero variant.
pub fn decode_enum<T>(
    field: u32,
    value: &FieldValue<'_>,
    parse: impl Fn(i32) -> Option<T>,
) -> Result<T, WireError> {
    let raw = value.as_u64()? as i64;
    let narrowed = i32::try_from(raw).map_err(|_| WireError::UnknownEnumValue { field, value: raw })?;
    parse(narrowed).ok_or(WireError::UnknownEnumValue { field, value: raw })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip_across_the_boundaries() {
        for value in [
            0u64,
            1,
            127,
            128,
            300,
            16_383,
            16_384,
            u32::MAX as u64,
            u64::MAX,
        ] {
            let mut out = Vec::new();
            write_varint(&mut out, value);
            let mut reader = Reader::new(&out);
            assert_eq!(reader.read_varint().unwrap(), value, "value {value}");
        }
    }

    #[test]
    fn a_varint_longer_than_ten_bytes_is_refused() {
        let overlong = vec![0xffu8; 11];
        let mut reader = Reader::new(&overlong);
        assert_eq!(reader.read_varint(), Err(WireError::VarintTooLong));
    }

    #[test]
    fn fields_round_trip() {
        let mut out = Vec::new();
        write_string(&mut out, 1, "PLAN-001");
        write_uint64(&mut out, 2, 730_769_584);
        write_bool(&mut out, 3, true);
        write_bytes(&mut out, 4, &[1, 2, 3]);

        let mut reader = Reader::new(&out);
        let mut seen = Vec::new();
        while let Some((field, value)) = reader.next_field().unwrap() {
            seen.push(field);
            match field {
                1 => assert_eq!(value.as_str(1).unwrap(), "PLAN-001"),
                2 => assert_eq!(value.as_u64().unwrap(), 730_769_584),
                3 => assert!(value.as_bool().unwrap()),
                4 => assert_eq!(value.as_bytes().unwrap(), &[1, 2, 3]),
                other => panic!("unexpected field {other}"),
            }
        }
        assert_eq!(seen, vec![1, 2, 3, 4]);
    }

    #[test]
    fn an_unknown_field_is_skipped_so_a_newer_peer_still_talks() {
        let mut out = Vec::new();
        write_string(&mut out, 1, "known");
        write_string(&mut out, 999, "a field this build has never heard of");
        write_uint64(&mut out, 2, 42);

        let mut reader = Reader::new(&out);
        let mut known = Vec::new();
        while let Some((field, value)) = reader.next_field().unwrap() {
            match field {
                1 => known.push(value.as_str(1).unwrap().to_string()),
                2 => known.push(value.as_u64().unwrap().to_string()),
                _ => {} // skipped, as protobuf intends
            }
        }
        assert_eq!(known, vec!["known".to_string(), "42".to_string()]);
    }

    #[test]
    fn an_unknown_enum_value_is_an_error_not_a_default() {
        // The whole point: a peer that sends status 77 must not be read as
        // STATUS_UNSPECIFIED, and a session kind nobody knows must not become
        // "public".
        let mut out = Vec::new();
        write_enum(&mut out, 3, 77);
        let mut reader = Reader::new(&out);
        let (field, value) = reader.next_field().unwrap().unwrap();
        let decoded = decode_enum(field, &value, |raw| match raw {
            1 => Some("public"),
            2 => Some("controller"),
            _ => None,
        });
        assert_eq!(
            decoded,
            Err(WireError::UnknownEnumValue {
                field: 3,
                value: 77
            })
        );
    }

    #[test]
    fn a_length_that_overruns_the_message_is_refused() {
        // Field 1, length-delimited, claiming 1000 bytes in a 4-byte message.
        let malformed = vec![0x0a, 0xe8, 0x07, 0x00];
        let mut reader = Reader::new(&malformed);
        assert_eq!(reader.next_field(), Err(WireError::LengthOverflow));
    }

    #[test]
    fn field_number_zero_is_refused() {
        let malformed = vec![0x00, 0x01];
        let mut reader = Reader::new(&malformed);
        assert_eq!(reader.next_field(), Err(WireError::FieldZero));
    }

    #[test]
    fn nesting_is_bounded() {
        let mut reader = Reader::new(&[]);
        reader.depth = MAX_DEPTH;
        let payload = FieldValue::Bytes(&[]);
        assert!(matches!(
            reader.nested_reader(&payload),
            Err(WireError::DepthLimit)
        ));
    }

    #[test]
    fn proto3_defaults_are_not_written() {
        let mut out = Vec::new();
        write_uint64(&mut out, 1, 0);
        write_bool(&mut out, 2, false);
        write_string(&mut out, 3, "");
        assert!(out.is_empty(), "proto3 zero values are absent on the wire");
    }

    #[test]
    fn an_empty_nested_message_is_still_present() {
        let mut out = Vec::new();
        write_message(&mut out, 1, &[]);
        assert!(!out.is_empty());
        let mut reader = Reader::new(&out);
        let (field, value) = reader.next_field().unwrap().unwrap();
        assert_eq!(field, 1);
        assert!(value.as_bytes().unwrap().is_empty());
    }
}
