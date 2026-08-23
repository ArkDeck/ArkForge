//! RFC 8949 §4.2 deterministically encoded CBOR — the security-digest model.
//!
//! Protobuf carries wire compatibility; this module carries meaning that has to
//! hash the same on both sides of the authority boundary (architecture.md 15.4).
//! The value model deliberately has no float, no host path and no indefinite
//! length: those are the shapes that make two honest peers disagree about a
//! digest.

use core::fmt;
use std::collections::BTreeSet;

/// A value in the canonical subset of CBOR that ArkForge digests.
///
/// Absent by design: floats (architecture.md 15.4 forbids them in the digest
/// model), tags, and indefinite-length items.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CborValue {
    /// Major type 0.
    Unsigned(u64),
    /// Major type 1. Holds the negative value itself, e.g. `-1`.
    Negative(i64),
    /// Major type 2.
    Bytes(Vec<u8>),
    /// Major type 3.
    Text(String),
    /// Major type 4.
    Array(Vec<CborValue>),
    /// Major type 5. Key order in this vector is irrelevant: the encoder sorts
    /// keys by their encoded bytes, per RFC 8949 §4.2.1.
    Map(Vec<(CborValue, CborValue)>),
    /// Major type 7, simple values 20/21.
    Bool(bool),
    /// Major type 7, simple value 22.
    Null,
}

impl CborValue {
    pub fn text(value: impl Into<String>) -> Self {
        CborValue::Text(value.into())
    }

    pub fn bytes(value: impl Into<Vec<u8>>) -> Self {
        CborValue::Bytes(value.into())
    }

    pub fn integer(value: i64) -> Self {
        if value < 0 {
            CborValue::Negative(value)
        } else {
            CborValue::Unsigned(value as u64)
        }
    }

    pub fn map(entries: Vec<(&str, CborValue)>) -> Self {
        CborValue::Map(
            entries
                .into_iter()
                .map(|(key, value)| (CborValue::text(key), value))
                .collect(),
        )
    }

    pub fn array(values: Vec<CborValue>) -> Self {
        CborValue::Array(values)
    }

    /// Encodes deterministically, or reports why the value cannot be canonical.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, CborError> {
        let mut out = Vec::new();
        self.encode_into(&mut out)?;
        Ok(out)
    }

    fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), CborError> {
        match self {
            CborValue::Unsigned(value) => {
                encode_head(out, 0, *value);
                Ok(())
            }
            CborValue::Negative(value) => {
                if *value >= 0 {
                    return Err(CborError::NegativeNotNegative(*value));
                }
                // RFC 8949 §3.1: major type 1 encodes -1 - n.
                let magnitude = (-(*value as i128) - 1) as u64;
                encode_head(out, 1, magnitude);
                Ok(())
            }
            CborValue::Bytes(value) => {
                encode_head(out, 2, value.len() as u64);
                out.extend_from_slice(value);
                Ok(())
            }
            CborValue::Text(value) => {
                encode_head(out, 3, value.len() as u64);
                out.extend_from_slice(value.as_bytes());
                Ok(())
            }
            CborValue::Array(values) => {
                encode_head(out, 4, values.len() as u64);
                for value in values {
                    value.encode_into(out)?;
                }
                Ok(())
            }
            CborValue::Map(entries) => {
                let mut encoded: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    let mut key_bytes = Vec::new();
                    key.encode_into(&mut key_bytes)?;
                    let mut value_bytes = Vec::new();
                    value.encode_into(&mut value_bytes)?;
                    encoded.push((key_bytes, value_bytes));
                }
                // RFC 8949 §4.2.1: sort by the encoded key, bytewise lexicographic.
                encoded.sort_by(|left, right| left.0.cmp(&right.0));
                let mut seen: BTreeSet<&Vec<u8>> = BTreeSet::new();
                for (key_bytes, _) in encoded.iter() {
                    if !seen.insert(key_bytes) {
                        return Err(CborError::DuplicateMapKey(key_bytes.clone()));
                    }
                }
                encode_head(out, 5, encoded.len() as u64);
                for (key_bytes, value_bytes) in encoded {
                    out.extend_from_slice(&key_bytes);
                    out.extend_from_slice(&value_bytes);
                }
                Ok(())
            }
            CborValue::Bool(value) => {
                out.push(0xe0 | if *value { 21 } else { 20 });
                Ok(())
            }
            CborValue::Null => {
                out.push(0xf6);
                Ok(())
            }
        }
    }
}

/// RFC 8949 §4.2.1 preferred serialization: the shortest head that fits.
fn encode_head(out: &mut Vec<u8>, major: u8, argument: u64) {
    let major = major << 5;
    if argument < 24 {
        out.push(major | argument as u8);
    } else if argument <= u8::MAX as u64 {
        out.push(major | 24);
        out.push(argument as u8);
    } else if argument <= u16::MAX as u64 {
        out.push(major | 25);
        out.extend_from_slice(&(argument as u16).to_be_bytes());
    } else if argument <= u32::MAX as u64 {
        out.push(major | 26);
        out.extend_from_slice(&(argument as u32).to_be_bytes());
    } else {
        out.push(major | 27);
        out.extend_from_slice(&argument.to_be_bytes());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CborError {
    /// The CBOR primitives are encodable, but the schema-bound model is not.
    ModelViolation(String),
    NegativeNotNegative(i64),
    DuplicateMapKey(Vec<u8>),
    Truncated,
    NonCanonicalHead,
    UnsupportedMajorType(u8),
    UnsupportedSimpleValue(u8),
    IndefiniteLength,
    UnsortedMapKeys,
    InvalidUtf8,
    TrailingBytes(usize),
    DepthLimit,
}

impl fmt::Display for CborError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CborError::ModelViolation(message) => write!(f, "model violation: {message}"),
            CborError::NegativeNotNegative(value) => {
                write!(
                    f,
                    "CborValue::Negative must hold a negative value, got {value}"
                )
            }
            CborError::DuplicateMapKey(key) => {
                write!(f, "duplicate map key in canonical encoding: {key:02x?}")
            }
            CborError::Truncated => f.write_str("truncated CBOR input"),
            CborError::NonCanonicalHead => {
                f.write_str("non-canonical head: argument not in preferred (shortest) form")
            }
            CborError::UnsupportedMajorType(major) => {
                write!(f, "unsupported CBOR major type {major}")
            }
            CborError::UnsupportedSimpleValue(value) => {
                write!(f, "unsupported CBOR simple value {value}")
            }
            CborError::IndefiniteLength => {
                f.write_str("indefinite-length items are not deterministic CBOR")
            }
            CborError::UnsortedMapKeys => f.write_str("map keys are not in canonical order"),
            CborError::InvalidUtf8 => f.write_str("text string is not valid UTF-8"),
            CborError::TrailingBytes(count) => write!(f, "{count} trailing bytes after value"),
            CborError::DepthLimit => f.write_str("nesting depth limit exceeded"),
        }
    }
}

impl std::error::Error for CborError {}

const MAX_DEPTH: usize = 64;

/// Decodes deterministic CBOR, rejecting anything that is merely *valid* CBOR.
///
/// This strictness is the point: a decoder that silently accepts a
/// non-canonical re-encoding of the same value would let two peers hold two
/// byte strings for one digest.
pub fn decode_canonical(input: &[u8]) -> Result<CborValue, CborError> {
    let mut cursor = Cursor { input, position: 0 };
    let value = cursor.read_value(0)?;
    if cursor.position != input.len() {
        return Err(CborError::TrailingBytes(input.len() - cursor.position));
    }
    Ok(value)
}

struct Cursor<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], CborError> {
        if self.position + count > self.input.len() {
            return Err(CborError::Truncated);
        }
        let slice = &self.input[self.position..self.position + count];
        self.position += count;
        Ok(slice)
    }

    fn read_head(&mut self) -> Result<(u8, u64), CborError> {
        let byte = *self.take(1)?.first().ok_or(CborError::Truncated)?;
        let major = byte >> 5;
        let additional = byte & 0x1f;
        let argument = match additional {
            0..=23 => additional as u64,
            24 => {
                let value = self.take(1)?[0] as u64;
                if value < 24 {
                    return Err(CborError::NonCanonicalHead);
                }
                value
            }
            25 => {
                let raw = self.take(2)?;
                let value = u16::from_be_bytes([raw[0], raw[1]]) as u64;
                if value <= u8::MAX as u64 {
                    return Err(CborError::NonCanonicalHead);
                }
                value
            }
            26 => {
                let raw = self.take(4)?;
                let value = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64;
                if value <= u16::MAX as u64 {
                    return Err(CborError::NonCanonicalHead);
                }
                value
            }
            27 => {
                let raw = self.take(8)?;
                let value = u64::from_be_bytes([
                    raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                ]);
                if value <= u32::MAX as u64 {
                    return Err(CborError::NonCanonicalHead);
                }
                value
            }
            31 => return Err(CborError::IndefiniteLength),
            _ => return Err(CborError::NonCanonicalHead),
        };
        Ok((major, argument))
    }

    fn read_value(&mut self, depth: usize) -> Result<CborValue, CborError> {
        if depth > MAX_DEPTH {
            return Err(CborError::DepthLimit);
        }
        let (major, argument) = self.read_head()?;
        match major {
            0 => Ok(CborValue::Unsigned(argument)),
            1 => {
                let value = -(argument as i128) - 1;
                if value < i64::MIN as i128 {
                    return Err(CborError::UnsupportedMajorType(1));
                }
                Ok(CborValue::Negative(value as i64))
            }
            2 => Ok(CborValue::Bytes(self.take(argument as usize)?.to_vec())),
            3 => {
                let raw = self.take(argument as usize)?;
                let text = std::str::from_utf8(raw).map_err(|_| CborError::InvalidUtf8)?;
                Ok(CborValue::Text(text.to_string()))
            }
            4 => {
                let mut values = Vec::new();
                for _ in 0..argument {
                    values.push(self.read_value(depth + 1)?);
                }
                Ok(CborValue::Array(values))
            }
            5 => {
                let mut entries = Vec::new();
                let mut previous_key: Option<Vec<u8>> = None;
                for _ in 0..argument {
                    let key_start = self.position;
                    let key = self.read_value(depth + 1)?;
                    let key_bytes = self.input[key_start..self.position].to_vec();
                    if let Some(previous) = previous_key.as_ref()
                        && previous >= &key_bytes
                    {
                        return Err(CborError::UnsortedMapKeys);
                    }
                    previous_key = Some(key_bytes);
                    let value = self.read_value(depth + 1)?;
                    entries.push((key, value));
                }
                Ok(CborValue::Map(entries))
            }
            7 => match argument {
                20 => Ok(CborValue::Bool(false)),
                21 => Ok(CborValue::Bool(true)),
                22 => Ok(CborValue::Null),
                other => Err(CborError::UnsupportedSimpleValue(other as u8)),
            },
            other => Err(CborError::UnsupportedMajorType(other)),
        }
    }
}

/// Types that have one canonical CBOR shape.
///
/// Implementations must be total and side-effect free: the same value must
/// produce the same CBOR on every host, in every process, forever.
pub trait CanonicalCbor {
    fn to_cbor(&self) -> CborValue;

    fn to_canonical_bytes(&self) -> Result<Vec<u8>, CborError> {
        self.to_cbor().to_canonical_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8949 Appendix A published examples.
    #[test]
    fn rfc8949_appendix_a_vectors() {
        let cases: Vec<(CborValue, &str)> = vec![
            (CborValue::Unsigned(0), "00"),
            (CborValue::Unsigned(1), "01"),
            (CborValue::Unsigned(10), "0a"),
            (CborValue::Unsigned(23), "17"),
            (CborValue::Unsigned(24), "1818"),
            (CborValue::Unsigned(25), "1819"),
            (CborValue::Unsigned(100), "1864"),
            (CborValue::Unsigned(1000), "1903e8"),
            (CborValue::Unsigned(1_000_000), "1a000f4240"),
            (CborValue::Unsigned(1_000_000_000_000), "1b000000e8d4a51000"),
            (CborValue::Negative(-1), "20"),
            (CborValue::Negative(-10), "29"),
            (CborValue::Negative(-100), "3863"),
            (CborValue::Negative(-1000), "3903e7"),
            (CborValue::Bool(false), "f4"),
            (CborValue::Bool(true), "f5"),
            (CborValue::Null, "f6"),
            (CborValue::Bytes(vec![]), "40"),
            (CborValue::Bytes(vec![1, 2, 3, 4]), "4401020304"),
            (CborValue::Text(String::new()), "60"),
            (CborValue::text("a"), "6161"),
            (CborValue::text("IETF"), "6449455446"),
            (CborValue::text("\"\\"), "62225c"),
            (CborValue::text("\u{00fc}"), "62c3bc"),
            (CborValue::Array(vec![]), "80"),
            (
                CborValue::Array(vec![
                    CborValue::Unsigned(1),
                    CborValue::Unsigned(2),
                    CborValue::Unsigned(3),
                ]),
                "83010203",
            ),
            (CborValue::Map(vec![]), "a0"),
            (
                CborValue::Map(vec![
                    (CborValue::Unsigned(1), CborValue::Unsigned(2)),
                    (CborValue::Unsigned(3), CborValue::Unsigned(4)),
                ]),
                "a201020304",
            ),
            (
                CborValue::Map(vec![
                    (CborValue::text("a"), CborValue::Unsigned(1)),
                    (
                        CborValue::text("b"),
                        CborValue::Array(vec![CborValue::Unsigned(2), CborValue::Unsigned(3)]),
                    ),
                ]),
                "a26161016162820203",
            ),
        ];
        for (value, expected) in cases {
            let encoded = value.to_canonical_bytes().unwrap();
            assert_eq!(hex(&encoded), expected, "value {value:?}");
            assert_eq!(decode_canonical(&encoded).unwrap(), value);
        }
    }

    #[test]
    fn map_keys_sort_by_encoded_bytes_not_by_text() {
        // RFC 8949 §4.2.1 sorts by encoded key, so shorter keys sort first
        // regardless of how the strings compare.
        let value = CborValue::Map(vec![
            (CborValue::text("bb"), CborValue::Unsigned(2)),
            (CborValue::text("a"), CborValue::Unsigned(1)),
        ]);
        let encoded = value.to_canonical_bytes().unwrap();
        // a2 | 61 61 -> "a", 01 | 62 62 62 -> "bb", 02
        assert_eq!(hex(&encoded), "a261610162626202");
        // Same map, opposite insertion order, identical bytes.
        let mirrored = CborValue::Map(vec![
            (CborValue::text("a"), CborValue::Unsigned(1)),
            (CborValue::text("bb"), CborValue::Unsigned(2)),
        ]);
        assert_eq!(mirrored.to_canonical_bytes().unwrap(), encoded);
    }

    #[test]
    fn duplicate_map_keys_are_rejected() {
        let value = CborValue::Map(vec![
            (CborValue::text("a"), CborValue::Unsigned(1)),
            (CborValue::text("a"), CborValue::Unsigned(2)),
        ]);
        assert!(matches!(
            value.to_canonical_bytes(),
            Err(CborError::DuplicateMapKey(_))
        ));
    }

    #[test]
    fn decoder_rejects_non_canonical_encodings() {
        // 1 encoded in a two-byte head.
        assert_eq!(
            decode_canonical(&[0x18, 0x01]),
            Err(CborError::NonCanonicalHead)
        );
        // Indefinite-length array.
        assert_eq!(
            decode_canonical(&[0x9f, 0x01, 0xff]),
            Err(CborError::IndefiniteLength)
        );
        // Map with keys out of canonical order: {"b":1,"a":2}.
        assert_eq!(
            decode_canonical(&[0xa2, 0x61, 0x62, 0x01, 0x61, 0x61, 0x02]),
            Err(CborError::UnsortedMapKeys)
        );
        // Half-precision float (major 7, argument 25) is not in the subset.
        assert!(matches!(
            decode_canonical(&[0xf9, 0x00, 0x00]),
            Err(CborError::NonCanonicalHead) | Err(CborError::UnsupportedSimpleValue(_))
        ));
        // Trailing bytes.
        assert_eq!(
            decode_canonical(&[0x01, 0x02]),
            Err(CborError::TrailingBytes(1))
        );
    }

    #[test]
    fn round_trip_is_byte_stable() {
        let value = CborValue::map(vec![
            ("planId", CborValue::text("PLAN-1")),
            (
                "steps",
                CborValue::array(vec![
                    CborValue::map(vec![("id", CborValue::text("STEP-1"))]),
                    CborValue::map(vec![("id", CborValue::text("STEP-2"))]),
                ]),
            ),
            ("count", CborValue::Unsigned(2)),
        ]);
        let first = value.to_canonical_bytes().unwrap();
        let decoded = decode_canonical(&first).unwrap();
        let second = decoded.to_canonical_bytes().unwrap();
        assert_eq!(first, second);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
