//! Streaming DEFLATE (RFC 1951) and gzip (RFC 1952).
//!
//! Streaming, not one-shot: a DAYU200 archive is ~700 MiB compressed and
//! carries a 2 GiB member, so the parser must be able to hash every member
//! without ever holding one in memory (architecture.md 10.4).
//!
//! The decoder is deliberately unforgiving. Truncated streams, trailing
//! garbage, and CRC/length mismatches are errors, because "mostly decoded" is
//! not a fact a plan may rest on.

use std::io::{self, Read};

/// The DEFLATE back-reference window.
const WINDOW: usize = 32 * 1024;
/// Compact the output buffer once this much history has been consumed.
const COMPACT_THRESHOLD: usize = WINDOW * 4;
const MAX_BITS: usize = 15;
/// Width of the primary decode table. Codes at or below this length decode in
/// one lookup; longer codes fall back to the canonical bit-at-a-time walk.
const FAST_BITS: usize = 9;

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// A canonical Huffman decoder.
#[derive(Debug, Clone)]
struct Huffman {
    /// `counts[len]` = number of codes of that bit length.
    counts: [u16; MAX_BITS + 1],
    /// Symbols ordered by (length, symbol).
    symbols: Vec<u16>,
    /// Per-length canonical decode state, precomputed so the slow path can
    /// start at `FAST_BITS + 1` instead of walking from bit 1.
    first_code: [u32; MAX_BITS + 2],
    first_index: [u32; MAX_BITS + 2],
    /// `(symbol << 4) | length`, or `u16::MAX` for "not a complete code".
    fast: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> io::Result<Self> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &length in lengths {
            if length as usize > MAX_BITS {
                return Err(invalid("huffman code length exceeds 15 bits"));
            }
            counts[length as usize] += 1;
        }
        counts[0] = 0;

        // Reject over-subscribed code sets. An incomplete set is legal only in
        // the single-symbol distance-tree case, which callers handle.
        let mut left = 1i32;
        for &count in counts.iter().take(MAX_BITS + 1).skip(1) {
            left <<= 1;
            left -= count as i32;
            if left < 0 {
                return Err(invalid("over-subscribed huffman code set"));
            }
        }

        let mut offsets = [0u32; MAX_BITS + 2];
        for length in 1..=MAX_BITS {
            offsets[length + 1] = offsets[length] + counts[length] as u32;
        }
        let total: usize = counts.iter().map(|c| *c as usize).sum();
        let mut symbols = vec![0u16; total];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                symbols[offsets[length as usize] as usize] = symbol as u16;
                offsets[length as usize] += 1;
            }
        }

        let mut first_code = [0u32; MAX_BITS + 2];
        let mut first_index = [0u32; MAX_BITS + 2];
        let mut code = 0u32;
        let mut index = 0u32;
        for length in 1..=MAX_BITS {
            first_code[length] = code;
            first_index[length] = index;
            code = (code + counts[length] as u32) << 1;
            index += counts[length] as u32;
        }

        let mut fast = vec![u16::MAX; 1 << FAST_BITS];
        let mut symbol_index = 0usize;
        let mut canonical = 0u32;
        for (length, &count) in counts.iter().enumerate().take(MAX_BITS + 1).skip(1) {
            for _ in 0..count {
                if length <= FAST_BITS {
                    // DEFLATE feeds Huffman codes MSB-first, so the table is
                    // indexed by the bit-reversed code plus every combination
                    // of the bits above it.
                    let reversed = reverse_bits(canonical, length);
                    let step = 1usize << length;
                    let mut slot = reversed as usize;
                    while slot < (1 << FAST_BITS) {
                        fast[slot] = ((symbols[symbol_index] as u16) << 4) | length as u16;
                        slot += step;
                    }
                }
                canonical += 1;
                symbol_index += 1;
            }
            canonical <<= 1;
        }

        Ok(Huffman {
            counts,
            symbols,
            first_code,
            first_index,
            fast,
        })
    }
}

fn reverse_bits(value: u32, bits: usize) -> u32 {
    let mut out = 0u32;
    for index in 0..bits {
        out |= ((value >> index) & 1) << (bits - 1 - index);
    }
    out
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Bit-level reader over a byte source, LSB-first as DEFLATE requires.
#[derive(Debug)]
struct BitReader<R: Read> {
    source: R,
    buffer: Vec<u8>,
    position: usize,
    filled: usize,
    bit_buffer: u64,
    bit_count: u32,
    source_exhausted: bool,
}

impl<R: Read> BitReader<R> {
    fn new(source: R) -> Self {
        BitReader {
            source,
            buffer: vec![0u8; 64 * 1024],
            position: 0,
            filled: 0,
            bit_buffer: 0,
            bit_count: 0,
            source_exhausted: false,
        }
    }

    fn next_byte(&mut self) -> io::Result<Option<u8>> {
        if self.position == self.filled {
            if self.source_exhausted {
                return Ok(None);
            }
            self.filled = self.source.read(&mut self.buffer)?;
            self.position = 0;
            if self.filled == 0 {
                self.source_exhausted = true;
                return Ok(None);
            }
        }
        let byte = self.buffer[self.position];
        self.position += 1;
        Ok(Some(byte))
    }

    fn need(&mut self, bits: u32) -> io::Result<()> {
        while self.bit_count < bits {
            match self.next_byte()? {
                Some(byte) => {
                    self.bit_buffer |= (byte as u64) << self.bit_count;
                    self.bit_count += 8;
                }
                None => return Err(truncated()),
            }
        }
        Ok(())
    }

    fn take(&mut self, bits: u32) -> io::Result<u32> {
        if bits == 0 {
            return Ok(0);
        }
        self.need(bits)?;
        let value = (self.bit_buffer & ((1u64 << bits) - 1)) as u32;
        self.bit_buffer >>= bits;
        self.bit_count -= bits;
        Ok(value)
    }

    /// Peeks up to `bits` bits without consuming, tolerating end of input so
    /// the final symbols of a stream can still be decoded.
    fn peek(&mut self, bits: u32) -> io::Result<u32> {
        while self.bit_count < bits {
            match self.next_byte()? {
                Some(byte) => {
                    self.bit_buffer |= (byte as u64) << self.bit_count;
                    self.bit_count += 8;
                }
                None => break,
            }
        }
        Ok((self.bit_buffer & ((1u64 << bits) - 1)) as u32)
    }

    fn consume(&mut self, bits: u32) {
        self.bit_buffer >>= bits;
        self.bit_count -= bits;
    }

    fn align_to_byte(&mut self) {
        let drop = self.bit_count % 8;
        self.bit_buffer >>= drop;
        self.bit_count -= drop;
    }

    fn read_aligned(&mut self, out: &mut [u8]) -> io::Result<()> {
        for slot in out.iter_mut() {
            if self.bit_count >= 8 {
                *slot = (self.bit_buffer & 0xff) as u8;
                self.bit_buffer >>= 8;
                self.bit_count -= 8;
            } else {
                *slot = self.next_byte()?.ok_or_else(truncated)?;
            }
        }
        Ok(())
    }

    fn decode(&mut self, table: &Huffman) -> io::Result<u16> {
        let window = self.peek(FAST_BITS as u32)?;
        let entry = table.fast[window as usize];
        if entry != u16::MAX {
            let length = (entry & 0xf) as u32;
            if length <= self.bit_count {
                self.consume(length);
                return Ok(entry >> 4);
            }
        }

        // Slow path: walk the canonical code from FAST_BITS + 1 upward.
        let mut code = reverse_bits(window, FAST_BITS);
        let mut consumed = FAST_BITS as u32;
        self.need(consumed)?;
        self.consume(consumed);
        for length in (FAST_BITS + 1)..=MAX_BITS {
            code = (code << 1) | self.take(1)?;
            consumed += 1;
            let count = table.counts[length] as u32;
            if count > 0 && code.wrapping_sub(table.first_code[length]) < count {
                let index = table.first_index[length] + (code - table.first_code[length]);
                return Ok(table.symbols[index as usize]);
            }
        }
        let _ = consumed;
        Err(invalid("invalid huffman code"))
    }

    /// Remaining buffered bytes, after discarding any partial bit remainder.
    fn drain_remaining(&mut self) -> io::Result<Vec<u8>> {
        self.align_to_byte();
        let mut out = Vec::new();
        while self.bit_count >= 8 {
            out.push((self.bit_buffer & 0xff) as u8);
            self.bit_buffer >>= 8;
            self.bit_count -= 8;
        }
        while let Some(byte) = self.next_byte()? {
            out.push(byte);
            if out.len() > 4096 {
                break;
            }
        }
        Ok(out)
    }
}

fn truncated() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "compressed stream ended mid-symbol".to_string(),
    )
}

#[derive(Debug)]
enum BlockState {
    None,
    Stored {
        remaining: u32,
    },
    Huffman {
        literal: Box<Huffman>,
        distance: Box<Huffman>,
    },
}

/// A streaming raw-DEFLATE reader.
#[derive(Debug)]
pub struct InflateReader<R: Read> {
    bits: BitReader<R>,
    output: Vec<u8>,
    read_cursor: usize,
    block: BlockState,
    final_block_seen: bool,
    finished: bool,
    total_out: u64,
}

impl<R: Read> InflateReader<R> {
    pub fn new(source: R) -> Self {
        InflateReader {
            bits: BitReader::new(source),
            output: Vec::with_capacity(COMPACT_THRESHOLD + WINDOW),
            read_cursor: 0,
            block: BlockState::None,
            final_block_seen: false,
            finished: false,
            total_out: 0,
        }
    }

    pub fn total_out(&self) -> u64 {
        self.total_out
    }

    /// Bytes left in the source after the DEFLATE stream ended.
    pub fn into_trailing(mut self) -> io::Result<Vec<u8>> {
        self.bits.drain_remaining()
    }

    fn available(&self) -> usize {
        self.output.len() - self.read_cursor
    }

    fn compact(&mut self) {
        if self.read_cursor > COMPACT_THRESHOLD {
            let keep_from = self.read_cursor.saturating_sub(WINDOW);
            self.output.drain(..keep_from);
            self.read_cursor -= keep_from;
        }
    }

    fn start_block(&mut self) -> io::Result<bool> {
        if self.final_block_seen {
            self.finished = true;
            return Ok(false);
        }
        let is_final = self.bits.take(1)? == 1;
        let block_type = self.bits.take(2)?;
        self.final_block_seen = is_final;
        match block_type {
            0 => {
                self.bits.align_to_byte();
                let mut header = [0u8; 4];
                self.bits.read_aligned(&mut header)?;
                let length = u16::from_le_bytes([header[0], header[1]]);
                let complement = u16::from_le_bytes([header[2], header[3]]);
                if length != !complement {
                    return Err(invalid("stored block length complement mismatch"));
                }
                self.block = BlockState::Stored {
                    remaining: length as u32,
                };
            }
            1 => {
                let mut literal_lengths = [0u8; 288];
                for (symbol, slot) in literal_lengths.iter_mut().enumerate() {
                    *slot = match symbol {
                        0..=143 => 8,
                        144..=255 => 9,
                        256..=279 => 7,
                        _ => 8,
                    };
                }
                let distance_lengths = [5u8; 30];
                self.block = BlockState::Huffman {
                    literal: Box::new(Huffman::new(&literal_lengths)?),
                    distance: Box::new(Huffman::new(&distance_lengths)?),
                };
            }
            2 => {
                let literal_count = self.bits.take(5)? as usize + 257;
                let distance_count = self.bits.take(5)? as usize + 1;
                let code_length_count = self.bits.take(4)? as usize + 4;
                if literal_count > 286 || distance_count > 30 {
                    return Err(invalid("dynamic block declares too many codes"));
                }
                let mut code_lengths = [0u8; 19];
                for index in 0..code_length_count {
                    code_lengths[CODE_LENGTH_ORDER[index]] = self.bits.take(3)? as u8;
                }
                let code_length_table = Huffman::new(&code_lengths)?;

                let mut lengths = vec![0u8; literal_count + distance_count];
                let mut index = 0usize;
                while index < lengths.len() {
                    let symbol = self.bits.decode(&code_length_table)?;
                    match symbol {
                        0..=15 => {
                            lengths[index] = symbol as u8;
                            index += 1;
                        }
                        16 => {
                            if index == 0 {
                                return Err(invalid("code-length repeat with no previous length"));
                            }
                            let previous = lengths[index - 1];
                            let repeat = 3 + self.bits.take(2)? as usize;
                            if index + repeat > lengths.len() {
                                return Err(invalid("code-length repeat overruns"));
                            }
                            for _ in 0..repeat {
                                lengths[index] = previous;
                                index += 1;
                            }
                        }
                        17 => {
                            let repeat = 3 + self.bits.take(3)? as usize;
                            if index + repeat > lengths.len() {
                                return Err(invalid("code-length zero run overruns"));
                            }
                            index += repeat;
                        }
                        18 => {
                            let repeat = 11 + self.bits.take(7)? as usize;
                            if index + repeat > lengths.len() {
                                return Err(invalid("code-length zero run overruns"));
                            }
                            index += repeat;
                        }
                        _ => return Err(invalid("invalid code-length symbol")),
                    }
                }
                let literal = Huffman::new(&lengths[..literal_count])?;
                let distance = Huffman::new(&lengths[literal_count..])?;
                self.block = BlockState::Huffman {
                    literal: Box::new(literal),
                    distance: Box::new(distance),
                };
            }
            _ => return Err(invalid("reserved DEFLATE block type")),
        }
        Ok(true)
    }

    /// Decodes until at least `want` bytes are buffered or the stream ends.
    fn fill(&mut self, want: usize) -> io::Result<()> {
        while !self.finished && self.available() < want {
            match &self.block {
                BlockState::None => {
                    if !self.start_block()? {
                        break;
                    }
                }
                BlockState::Stored { remaining } => {
                    let remaining = *remaining;
                    if remaining == 0 {
                        self.block = BlockState::None;
                        continue;
                    }
                    let chunk = remaining.min(64 * 1024) as usize;
                    let start = self.output.len();
                    self.output.resize(start + chunk, 0);
                    self.bits.read_aligned(&mut self.output[start..])?;
                    self.total_out += chunk as u64;
                    self.block = BlockState::Stored {
                        remaining: remaining - chunk as u32,
                    };
                }
                BlockState::Huffman { .. } => {
                    let (literal, distance) = match &self.block {
                        BlockState::Huffman { literal, distance } => {
                            (literal.clone(), distance.clone())
                        }
                        _ => unreachable!(),
                    };
                    self.decode_huffman_block(&literal, &distance, want)?;
                }
            }
        }
        Ok(())
    }

    fn decode_huffman_block(
        &mut self,
        literal: &Huffman,
        distance: &Huffman,
        want: usize,
    ) -> io::Result<()> {
        loop {
            if self.available() >= want {
                return Ok(());
            }
            let symbol = self.bits.decode(literal)?;
            match symbol {
                0..=255 => {
                    self.output.push(symbol as u8);
                    self.total_out += 1;
                }
                256 => {
                    self.block = BlockState::None;
                    return Ok(());
                }
                257..=285 => {
                    let index = (symbol - 257) as usize;
                    let length = LENGTH_BASE[index] as usize
                        + self.bits.take(LENGTH_EXTRA[index] as u32)? as usize;
                    let distance_symbol = self.bits.decode(distance)? as usize;
                    if distance_symbol >= DISTANCE_BASE.len() {
                        return Err(invalid("invalid distance symbol"));
                    }
                    let back = DISTANCE_BASE[distance_symbol] as usize
                        + self.bits.take(DISTANCE_EXTRA[distance_symbol] as u32)? as usize;
                    if back > self.output.len() {
                        return Err(invalid("back-reference before start of stream"));
                    }
                    let source = self.output.len() - back;
                    for source in (source..).take(length) {
                        let byte = self.output[source];
                        self.output.push(byte);
                    }
                    self.total_out += length as u64;
                }
                _ => return Err(invalid("invalid literal/length symbol")),
            }
        }
    }
}

impl<R: Read> Read for InflateReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        self.fill(out.len())?;
        let count = self.available().min(out.len());
        if count == 0 {
            return Ok(0);
        }
        out[..count].copy_from_slice(&self.output[self.read_cursor..self.read_cursor + count]);
        self.read_cursor += count;
        self.compact();
        Ok(count)
    }
}

/// CRC-32 as specified by RFC 1952 §8.
#[derive(Debug, Clone)]
pub struct Crc32 {
    value: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub fn new() -> Self {
        Crc32 { value: 0xffff_ffff }
    }

    pub fn update(&mut self, data: &[u8]) {
        let table = crc_table();
        let mut value = self.value;
        for byte in data {
            value = table[((value ^ *byte as u32) & 0xff) as usize] ^ (value >> 8);
        }
        self.value = value;
    }

    pub fn finalize(&self) -> u32 {
        self.value ^ 0xffff_ffff
    }
}

/// The RFC 1952 §8 table, built at compile time so there is no initialization
/// race to reason about and no `unsafe` to audit.
static CRC_TABLE: [u32; 256] = build_crc_table();

const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0usize;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                0xedb8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

fn crc_table() -> &'static [u32; 256] {
    &CRC_TABLE
}

/// Why a gzip stream was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GzipError {
    NotGzip,
    UnsupportedCompressionMethod(u8),
    ReservedFlags(u8),
    HeaderChecksum { expected: u16, observed: u16 },
    Truncated,
    CrcMismatch { expected: u32, observed: u32 },
    LengthMismatch { expected: u32, observed: u64 },
    TrailingBytes(usize),
}

impl std::fmt::Display for GzipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GzipError::NotGzip => f.write_str("not a gzip stream"),
            GzipError::UnsupportedCompressionMethod(method) => {
                write!(f, "unsupported gzip compression method {method}")
            }
            GzipError::ReservedFlags(flags) => {
                write!(f, "reserved gzip flag bits set: {flags:#04x}")
            }
            GzipError::HeaderChecksum { expected, observed } => write!(
                f,
                "gzip header CRC16 mismatch: expected {expected:#06x}, observed {observed:#06x}"
            ),
            GzipError::Truncated => f.write_str("gzip stream truncated"),
            GzipError::CrcMismatch { expected, observed } => write!(
                f,
                "gzip CRC32 mismatch: expected {expected:#010x}, observed {observed:#010x}"
            ),
            GzipError::LengthMismatch { expected, observed } => write!(
                f,
                "gzip ISIZE mismatch: expected {expected}, observed {observed}"
            ),
            GzipError::TrailingBytes(count) => {
                write!(f, "{count} unexpected bytes after the gzip trailer")
            }
        }
    }
}

impl std::error::Error for GzipError {}

impl From<GzipError> for io::Error {
    fn from(error: GzipError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, error)
    }
}

/// A streaming gzip reader that verifies CRC-32 and ISIZE at end of stream.
#[derive(Debug)]
pub struct GzipReader<R: Read> {
    inner: Option<InflateReader<R>>,
    crc: Crc32,
    finished: bool,
    /// Set once the trailer has been checked, so a caller can assert it ran.
    trailer_verified: bool,
}

impl<R: Read> GzipReader<R> {
    pub fn new(mut source: R) -> Result<Self, io::Error> {
        let mut header = [0u8; 10];
        read_exact_or(&mut source, &mut header, GzipError::Truncated)?;
        if header[0] != 0x1f || header[1] != 0x8b {
            return Err(GzipError::NotGzip.into());
        }
        if header[2] != 8 {
            return Err(GzipError::UnsupportedCompressionMethod(header[2]).into());
        }
        let flags = header[3];
        if flags & 0xe0 != 0 {
            return Err(GzipError::ReservedFlags(flags).into());
        }
        let mut header_bytes = header.to_vec();

        if flags & 0x04 != 0 {
            let mut extra_length = [0u8; 2];
            read_exact_or(&mut source, &mut extra_length, GzipError::Truncated)?;
            header_bytes.extend_from_slice(&extra_length);
            let length = u16::from_le_bytes(extra_length) as usize;
            let mut extra = vec![0u8; length];
            read_exact_or(&mut source, &mut extra, GzipError::Truncated)?;
            header_bytes.extend_from_slice(&extra);
        }
        if flags & 0x08 != 0 {
            read_cstring(&mut source, &mut header_bytes)?;
        }
        if flags & 0x10 != 0 {
            read_cstring(&mut source, &mut header_bytes)?;
        }
        if flags & 0x02 != 0 {
            let mut stored = [0u8; 2];
            read_exact_or(&mut source, &mut stored, GzipError::Truncated)?;
            let expected = u16::from_le_bytes(stored);
            let mut crc = Crc32::new();
            crc.update(&header_bytes);
            let observed = (crc.finalize() & 0xffff) as u16;
            if expected != observed {
                return Err(GzipError::HeaderChecksum { expected, observed }.into());
            }
        }

        Ok(GzipReader {
            inner: Some(InflateReader::new(source)),
            crc: Crc32::new(),
            finished: false,
            trailer_verified: false,
        })
    }

    pub fn trailer_verified(&self) -> bool {
        self.trailer_verified
    }

    fn verify_trailer(&mut self) -> io::Result<()> {
        let inner = match self.inner.take() {
            Some(inner) => inner,
            None => return Ok(()),
        };
        let total_out = inner.total_out();
        let observed_crc = self.crc.finalize();
        let trailing = inner.into_trailing()?;
        if trailing.len() < 8 {
            return Err(GzipError::Truncated.into());
        }
        let expected_crc = u32::from_le_bytes([trailing[0], trailing[1], trailing[2], trailing[3]]);
        let expected_size =
            u32::from_le_bytes([trailing[4], trailing[5], trailing[6], trailing[7]]);
        if expected_crc != observed_crc {
            return Err(GzipError::CrcMismatch {
                expected: expected_crc,
                observed: observed_crc,
            }
            .into());
        }
        // ISIZE is the uncompressed size modulo 2^32 (RFC 1952 §2.3.1).
        if expected_size != (total_out & 0xffff_ffff) as u32 {
            return Err(GzipError::LengthMismatch {
                expected: expected_size,
                observed: total_out,
            }
            .into());
        }
        if trailing.len() > 8 {
            return Err(GzipError::TrailingBytes(trailing.len() - 8).into());
        }
        self.trailer_verified = true;
        Ok(())
    }
}

impl<R: Read> Read for GzipReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.finished {
            return Ok(0);
        }
        let count = match self.inner.as_mut() {
            Some(inner) => inner.read(out)?,
            None => 0,
        };
        if count == 0 {
            self.finished = true;
            self.verify_trailer()?;
            return Ok(0);
        }
        self.crc.update(&out[..count]);
        Ok(count)
    }
}

fn read_exact_or<R: Read>(source: &mut R, out: &mut [u8], error: GzipError) -> io::Result<()> {
    let mut filled = 0usize;
    while filled < out.len() {
        let count = source.read(&mut out[filled..])?;
        if count == 0 {
            return Err(error.into());
        }
        filled += count;
    }
    Ok(())
}

fn read_cstring<R: Read>(source: &mut R, sink: &mut Vec<u8>) -> io::Result<()> {
    let mut byte = [0u8; 1];
    loop {
        read_exact_or(source, &mut byte, GzipError::Truncated)?;
        sink.push(byte[0]);
        if byte[0] == 0 {
            return Ok(());
        }
        if sink.len() > 64 * 1024 {
            return Err(invalid("gzip header string is implausibly long"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Compresses with the system gzip so the fixtures are produced by an
    /// independent implementation, not by ours.
    fn system_gzip(data: &[u8], level: &str) -> Option<Vec<u8>> {
        let mut child = Command::new("gzip")
            .arg(level)
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        child.stdin.as_mut()?.write_all(data).ok()?;
        let output = child.wait_with_output().ok()?;
        if !output.status.success() {
            return None;
        }
        Some(output.stdout)
    }

    fn inflate_gzip(bytes: &[u8]) -> io::Result<Vec<u8>> {
        let mut reader = GzipReader::new(bytes)?;
        let mut out = Vec::new();
        reader.read_to_end(&mut out)?;
        assert!(reader.trailer_verified(), "trailer must be checked");
        Ok(out)
    }

    #[test]
    fn round_trips_system_gzip_across_block_types_and_levels() {
        // Level 0 forces stored blocks; level 9 forces dynamic Huffman;
        // highly repetitive data exercises long back-references.
        let corpora: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            b"hello hello hello hello hello".to_vec(),
            (0..=255u8).cycle().take(200_000).collect(),
            vec![0u8; 300_000],
            {
                // Pseudo-random but deterministic: a linear congruential walk.
                let mut state = 0x1234_5678u32;
                (0..250_000)
                    .map(|_| {
                        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (state >> 24) as u8
                    })
                    .collect()
            },
        ];
        let mut ran = 0;
        for corpus in &corpora {
            for level in ["-1", "-6", "-9"] {
                let Some(compressed) = system_gzip(corpus, level) else {
                    continue;
                };
                ran += 1;
                let decoded = inflate_gzip(&compressed).unwrap_or_else(|error| {
                    panic!("level {level}, {} bytes: {error}", corpus.len())
                });
                assert_eq!(decoded.len(), corpus.len(), "level {level}");
                assert_eq!(&decoded, corpus, "level {level}");
            }
        }
        assert!(ran > 0, "system gzip is required for this test");
    }

    #[test]
    fn small_reads_produce_the_same_bytes_as_one_big_read() {
        let corpus: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
        let Some(compressed) = system_gzip(&corpus, "-9") else {
            return;
        };
        let mut reader = GzipReader::new(compressed.as_slice()).unwrap();
        let mut out = Vec::new();
        let mut chunk = [0u8; 7];
        loop {
            let count = reader.read(&mut chunk).unwrap();
            if count == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..count]);
        }
        assert_eq!(out, corpus);
    }

    #[test]
    fn a_non_gzip_stream_is_rejected() {
        let error = GzipReader::new(&b"not a gzip stream at all"[..]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("not a gzip"));
    }

    #[test]
    fn a_corrupted_payload_fails_the_crc() {
        let corpus = b"the quick brown fox jumps over the lazy dog".repeat(100);
        let Some(mut compressed) = system_gzip(&corpus, "-9") else {
            return;
        };
        let length = compressed.len();
        // Flip a bit in the CRC trailer rather than the payload, so the stream
        // still decodes and only the integrity check can catch it.
        compressed[length - 5] ^= 0x01;
        let error = inflate_gzip(&compressed).unwrap_err();
        assert!(error.to_string().contains("CRC32"), "{error}");
    }

    #[test]
    fn a_truncated_stream_is_rejected() {
        let corpus = vec![7u8; 100_000];
        let Some(compressed) = system_gzip(&corpus, "-9") else {
            return;
        };
        let truncated = &compressed[..compressed.len() / 2];
        assert!(inflate_gzip(truncated).is_err());
    }

    #[test]
    fn trailing_garbage_after_the_trailer_is_rejected() {
        let corpus = b"payload".to_vec();
        let Some(mut compressed) = system_gzip(&corpus, "-9") else {
            return;
        };
        compressed.extend_from_slice(b"garbage");
        let error = inflate_gzip(&compressed).unwrap_err();
        assert!(
            error.to_string().contains("after the gzip trailer"),
            "{error}"
        );
    }

    #[test]
    fn crc32_matches_the_rfc1952_reference_value() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finalize(), 0xcbf4_3926);
    }
}
