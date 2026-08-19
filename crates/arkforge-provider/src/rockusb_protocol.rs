//! Native RockUSB protocol.
//!
//! The byte grammar is pinned to
//! `rockchip-linux/rkdeveloptool@304f073752fd25c854e1bcf05d8e7f925b1f4e14`:
//! `RKComm.h` defines the 31-byte CBW and 13-byte CSW; `RKComm.cpp` defines
//! TEST_UNIT_READY, READ_LBA, WRITE_LBA, READ_FLASH_INFO and DEVICE_RESET.
//! This module owns only that pure byte protocol. Claiming USB and moving bytes
//! are separate concerns.

use arkforge_artifact::manifest::{GrammarBranch, PartitionEntryFact, PartitionTableFact};
use core::fmt;

pub const LOGICAL_BLOCK_BYTES: usize = 512;
pub const ROCKUSB_TRANSFER_CHUNK_SECTORS: u16 = 128;

const CBW_BYTES: usize = 31;
const CSW_BYTES: usize = 13;
const CBW_SIGNATURE: [u8; 4] = *b"USBC";
const CSW_SIGNATURE: [u8; 4] = *b"USBS";
const TEST_UNIT_READY: u8 = 0x00;
const READ_LBA: u8 = 0x14;
const WRITE_LBA: u8 = 0x15;
const READ_FLASH_INFO: u8 = 0x1a;
const DEVICE_RESET: u8 = 0xff;
const DEVICE_STRING: &str = "rk29xxnand";

/// The safe transport surface consumed by the protocol engine.
pub trait RockUsbBulkIo: fmt::Debug {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn read_some(&mut self, bytes: &mut [u8]) -> Result<usize, String>;
    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), String>;
}

/// The native protocol engine. A fresh instance owns one exclusively claimed USB
/// interface, so tags and CSWs cannot be interleaved with another caller.
#[derive(Debug)]
pub struct RockUsbProtocol<'a> {
    io: &'a mut dyn RockUsbBulkIo,
    next_tag: u32,
}

/// Structured progress from native WRITE_LBA. `payload_bytes` excludes the
/// zero padding in the final sector; `wire_sectors` includes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RockUsbTransferProgress {
    pub payload_bytes: u64,
    pub wire_sectors: u64,
    pub chunks: u64,
}

impl<'a> RockUsbProtocol<'a> {
    pub fn new(io: &'a mut dyn RockUsbBulkIo, first_tag: u32) -> Self {
        Self {
            io,
            next_tag: first_tag,
        }
    }

    /// Loader readiness is a protocol answer, not a VID/PID inference.
    pub fn test_unit_ready(&mut self) -> Result<(), RockUsbProtocolError> {
        self.execute_in(TEST_UNIT_READY, 0, 0, 0, 6)?;
        Ok(())
    }

    /// Sector capacity from the vendor protocol's READ_FLASH_INFO response.
    /// The pinned implementation reads the first native-endian DWORD; its
    /// supported hosts are little-endian, so the wire interpretation is LE.
    pub fn read_capacity_sectors(&mut self) -> Result<u64, RockUsbProtocolError> {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        let cbw = command_block(tag, READ_FLASH_INFO, 0, 0, 11, 6, true);
        self.io
            .write_all(&cbw)
            .map_err(RockUsbProtocolError::Transport)?;
        // The pinned RKU_ReadFlashInfo supplies 512 bytes to RKU_Read_EX and
        // accepts any data stage from 11 through 512 bytes. Preserve that
        // tolerance without accepting an unbounded or undersized answer.
        let mut bytes = vec![0u8; LOGICAL_BLOCK_BYTES];
        let actual = self
            .io
            .read_some(&mut bytes)
            .map_err(RockUsbProtocolError::Transport)?;
        if !(11..=LOGICAL_BLOCK_BYTES).contains(&actual) {
            return Err(RockUsbProtocolError::MalformedResponse(format!(
                "READ_FLASH_INFO returned {actual} bytes, expected 11..={LOGICAL_BLOCK_BYTES}"
            )));
        }
        let mut csw = [0u8; CSW_BYTES];
        self.io
            .read_exact(&mut csw)
            .map_err(RockUsbProtocolError::Transport)?;
        validate_csw(&csw, tag)?;
        let sectors = u32::from_le_bytes(bytes[0..4].try_into().expect("four bytes"));
        if sectors == 0 {
            return Err(RockUsbProtocolError::MalformedResponse(
                "READ_FLASH_INFO reported zero sectors".into(),
            ));
        }
        Ok(sectors as u64)
    }

    /// Reads an exact sector range using 128-sector RockUSB transfer chunks.
    pub fn read_lba(
        &mut self,
        begin_sector: u64,
        sectors: u64,
    ) -> Result<Vec<u8>, RockUsbProtocolError> {
        let end =
            begin_sector
                .checked_add(sectors)
                .ok_or(RockUsbProtocolError::AddressOutOfRange {
                    begin_sector,
                    sectors,
                })?;
        if begin_sector > u32::MAX as u64 || end > u32::MAX as u64 + 1 {
            return Err(RockUsbProtocolError::AddressOutOfRange {
                begin_sector,
                sectors,
            });
        }
        let byte_count = sectors
            .checked_mul(LOGICAL_BLOCK_BYTES as u64)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or(RockUsbProtocolError::TransferTooLarge { sectors })?;
        let mut all = Vec::with_capacity(byte_count);
        let mut position = begin_sector;
        let mut remaining = sectors;
        while remaining > 0 {
            let chunk = remaining.min(ROCKUSB_TRANSFER_CHUNK_SECTORS as u64) as u16;
            let transfer_bytes = chunk as usize * LOGICAL_BLOCK_BYTES;
            let bytes =
                self.execute_in(READ_LBA, position as u32, chunk, transfer_bytes as u32, 10)?;
            all.extend_from_slice(&bytes);
            position += chunk as u64;
            remaining -= chunk as u64;
        }
        Ok(all)
    }

    /// Writes bytes at an exact LBA using 128-sector RockUSB chunks. The final
    /// partial sector is zero-filled, matching `write_lba`.
    pub fn write_lba(
        &mut self,
        begin_sector: u64,
        bytes: &[u8],
    ) -> Result<RockUsbTransferProgress, RockUsbProtocolError> {
        if bytes.is_empty() {
            return Err(RockUsbProtocolError::EmptyWrite);
        }
        let sectors = ceil_div(bytes.len(), LOGICAL_BLOCK_BYTES) as u64;
        validate_sector_range(begin_sector, sectors)?;

        let mut position = begin_sector;
        let mut offset = 0usize;
        let mut chunks = 0u64;
        let max_chunk_bytes = ROCKUSB_TRANSFER_CHUNK_SECTORS as usize * LOGICAL_BLOCK_BYTES;
        while offset < bytes.len() {
            let payload_end = (offset + max_chunk_bytes).min(bytes.len());
            let payload = &bytes[offset..payload_end];
            let chunk_sectors = ceil_div(payload.len(), LOGICAL_BLOCK_BYTES) as u16;
            let wire_bytes = chunk_sectors as usize * LOGICAL_BLOCK_BYTES;
            let mut padded = Vec::new();
            let data = if payload.len() == wire_bytes {
                payload
            } else {
                padded.resize(wire_bytes, 0);
                padded[..payload.len()].copy_from_slice(payload);
                &padded
            };
            self.execute_out(WRITE_LBA, position as u32, chunk_sectors, data, 10)?;
            position += chunk_sectors as u64;
            offset = payload_end;
            chunks += 1;
        }
        Ok(RockUsbTransferProgress {
            payload_bytes: bytes.len() as u64,
            wire_sectors: sectors,
            chunks,
        })
    }

    /// Reboots the Loader after its CSW confirms DEVICE_RESET.
    pub fn reset_device(&mut self) -> Result<(), RockUsbProtocolError> {
        self.execute_out(DEVICE_RESET, 0, 0, &[], 6)
    }

    /// Reads and validates the device's primary GPT, then projects it into
    /// ArkForge partition semantics.
    pub fn read_partition_table(&mut self) -> Result<PartitionTableFact, RockUsbProtocolError> {
        let header_block = self.read_lba(1, 1)?;
        let header = GptHeader::parse(&header_block)?;
        let entries_bytes = (header.entry_count as usize)
            .checked_mul(header.entry_size as usize)
            .ok_or_else(|| {
                RockUsbProtocolError::MalformedGpt("partition entry byte count overflows".into())
            })?;
        if entries_bytes > 16 * 1024 * 1024 {
            return Err(RockUsbProtocolError::MalformedGpt(format!(
                "partition entry array is {entries_bytes} bytes"
            )));
        }
        let entry_sectors = ceil_div(entries_bytes, LOGICAL_BLOCK_BYTES);
        let entry_bytes = self.read_lba(header.entry_lba, entry_sectors as u64)?;
        parse_partition_entries(&header, &entry_bytes[..entries_bytes])
    }

    fn execute_in(
        &mut self,
        opcode: u8,
        address: u32,
        sectors: u16,
        transfer_bytes: u32,
        command_length: u8,
    ) -> Result<Vec<u8>, RockUsbProtocolError> {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        let cbw = command_block(
            tag,
            opcode,
            address,
            sectors,
            transfer_bytes,
            command_length,
            true,
        );
        self.io
            .write_all(&cbw)
            .map_err(RockUsbProtocolError::Transport)?;
        let mut data = vec![0u8; transfer_bytes as usize];
        if !data.is_empty() {
            self.io
                .read_exact(&mut data)
                .map_err(RockUsbProtocolError::Transport)?;
        }
        let mut csw = [0u8; CSW_BYTES];
        self.io
            .read_exact(&mut csw)
            .map_err(RockUsbProtocolError::Transport)?;
        validate_csw(&csw, tag)?;
        Ok(data)
    }

    fn execute_out(
        &mut self,
        opcode: u8,
        address: u32,
        sectors: u16,
        data: &[u8],
        command_length: u8,
    ) -> Result<(), RockUsbProtocolError> {
        let transfer_bytes = u32::try_from(data.len())
            .map_err(|_| RockUsbProtocolError::TransferTooLarge {
                sectors: sectors as u64,
            })?;
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        let cbw = command_block(
            tag,
            opcode,
            address,
            sectors,
            transfer_bytes,
            command_length,
            false,
        );
        self.io
            .write_all(&cbw)
            .map_err(RockUsbProtocolError::Transport)?;
        if !data.is_empty() {
            self.io
                .write_all(data)
                .map_err(RockUsbProtocolError::Transport)?;
        }
        let mut csw = [0u8; CSW_BYTES];
        self.io
            .read_exact(&mut csw)
            .map_err(RockUsbProtocolError::Transport)?;
        validate_csw(&csw, tag)
    }
}

fn command_block(
    tag: u32,
    opcode: u8,
    address: u32,
    sectors: u16,
    transfer_bytes: u32,
    command_length: u8,
    direction_in: bool,
) -> [u8; CBW_BYTES] {
    let mut cbw = [0u8; CBW_BYTES];
    cbw[0..4].copy_from_slice(&CBW_SIGNATURE);
    cbw[4..8].copy_from_slice(&tag.to_le_bytes());
    cbw[8..12].copy_from_slice(&transfer_bytes.to_le_bytes());
    cbw[12] = if direction_in { 0x80 } else { 0x00 };
    cbw[13] = 0; // LUN
    cbw[14] = command_length;
    cbw[15] = opcode;
    // byte 16 is the read-method subcode (RWMETHOD_IMAGE = 0)
    cbw[17..21].copy_from_slice(&address.to_be_bytes());
    cbw[22..24].copy_from_slice(&sectors.to_be_bytes());
    cbw
}

fn validate_sector_range(
    begin_sector: u64,
    sectors: u64,
) -> Result<(), RockUsbProtocolError> {
    let end = begin_sector
        .checked_add(sectors)
        .ok_or(RockUsbProtocolError::AddressOutOfRange {
            begin_sector,
            sectors,
        })?;
    if begin_sector > u32::MAX as u64 || end > u32::MAX as u64 + 1 {
        return Err(RockUsbProtocolError::AddressOutOfRange {
            begin_sector,
            sectors,
        });
    }
    Ok(())
}

fn validate_csw(bytes: &[u8; CSW_BYTES], expected_tag: u32) -> Result<(), RockUsbProtocolError> {
    if bytes[0..4] != CSW_SIGNATURE {
        return Err(RockUsbProtocolError::CswSignature(
            bytes[0..4].try_into().unwrap(),
        ));
    }
    let tag = u32::from_le_bytes(bytes[4..8].try_into().expect("four bytes"));
    if tag != expected_tag {
        return Err(RockUsbProtocolError::CswTag {
            expected: expected_tag,
            observed: tag,
        });
    }
    if bytes[12] != 0 {
        return Err(RockUsbProtocolError::CommandFailed {
            status: bytes[12],
            residue: u32::from_le_bytes(bytes[8..12].try_into().expect("four bytes")),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GptHeader {
    entry_lba: u64,
    entry_count: u32,
    entry_size: u32,
    entry_crc32: u32,
}

impl GptHeader {
    fn parse(block: &[u8]) -> Result<Self, RockUsbProtocolError> {
        if block.len() != LOGICAL_BLOCK_BYTES || &block[0..8] != b"EFI PART" {
            return Err(RockUsbProtocolError::MalformedGpt(
                "LBA 1 is not a 512-byte EFI PART header".into(),
            ));
        }
        let header_size = le_u32(block, 12)? as usize;
        if !(92..=LOGICAL_BLOCK_BYTES).contains(&header_size) {
            return Err(RockUsbProtocolError::MalformedGpt(format!(
                "header size {header_size} is outside 92..=512"
            )));
        }
        let expected_crc = le_u32(block, 16)?;
        let mut covered = block[..header_size].to_vec();
        covered[16..20].fill(0);
        let observed_crc = crc32(&covered);
        if observed_crc != expected_crc {
            return Err(RockUsbProtocolError::MalformedGpt(format!(
                "header CRC32 is {observed_crc:08x}, expected {expected_crc:08x}"
            )));
        }
        let entry_lba = le_u64(block, 72)?;
        let entry_count = le_u32(block, 80)?;
        let entry_size = le_u32(block, 84)?;
        let entry_crc32 = le_u32(block, 88)?;
        if entry_lba < 2 || entry_count == 0 || entry_size < 128 || entry_size % 8 != 0 {
            return Err(RockUsbProtocolError::MalformedGpt(format!(
                "invalid entry geometry lba={entry_lba} count={entry_count} size={entry_size}"
            )));
        }
        Ok(Self {
            entry_lba,
            entry_count,
            entry_size,
            entry_crc32,
        })
    }
}

fn parse_partition_entries(
    header: &GptHeader,
    bytes: &[u8],
) -> Result<PartitionTableFact, RockUsbProtocolError> {
    let observed_crc = crc32(bytes);
    if observed_crc != header.entry_crc32 {
        return Err(RockUsbProtocolError::MalformedGpt(format!(
            "partition array CRC32 is {observed_crc:08x}, expected {:08x}",
            header.entry_crc32
        )));
    }
    let entry_size = header.entry_size as usize;
    let mut rows: Vec<(u32, u64, String)> = Vec::new();
    for index in 0..header.entry_count as usize {
        let start = index * entry_size;
        let entry = &bytes[start..start + entry_size];
        if entry[0..16].iter().all(|byte| *byte == 0) {
            continue;
        }
        let first_lba = le_u64(entry, 32)?;
        let last_lba = le_u64(entry, 40)?;
        if first_lba == 0 || last_lba < first_lba {
            return Err(RockUsbProtocolError::MalformedGpt(format!(
                "entry {index} has range {first_lba}..={last_lba}"
            )));
        }
        let name_bytes = entry.get(56..128).ok_or_else(|| {
            RockUsbProtocolError::MalformedGpt(format!("entry {index} has no UTF-16 name"))
        })?;
        let mut units = Vec::new();
        for pair in name_bytes.chunks_exact(2) {
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
        let name = String::from_utf16(&units).map_err(|_| {
            RockUsbProtocolError::MalformedGpt(format!("entry {index} name is invalid UTF-16"))
        })?;
        if name.is_empty() {
            return Err(RockUsbProtocolError::MalformedGpt(format!(
                "entry {index} has an empty name"
            )));
        }
        if let Some((_, previous, _)) = rows.last() {
            if first_lba <= *previous {
                return Err(RockUsbProtocolError::MalformedGpt(format!(
                    "entry {index} begins at {first_lba}, not after {previous}"
                )));
            }
        }
        rows.push((index as u32, first_lba, name));
    }
    if rows.is_empty() {
        return Err(RockUsbProtocolError::MalformedGpt(
            "partition array contains no entries".into(),
        ));
    }
    let mut entries = Vec::with_capacity(rows.len());
    for (position, (index, offset, name)) in rows.iter().enumerate() {
        let size_sectors = rows
            .get(position + 1)
            .map(|(_, next_offset, _)| next_offset - offset);
        entries.push(PartitionEntryFact {
            index: *index,
            name: name.clone(),
            offset_sectors: *offset,
            size_sectors,
            attribute: None,
            grammar_branch: if size_sectors.is_some() {
                GrammarBranch::Fixed
            } else {
                GrammarBranch::RemainderGrow
            },
        });
    }
    Ok(PartitionTableFact {
        device: DEVICE_STRING.into(),
        logical_block_size: LOGICAL_BLOCK_BYTES as u32,
        entries,
    })
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32, RockUsbProtocolError> {
    let value = bytes.get(offset..offset + 4).ok_or_else(|| {
        RockUsbProtocolError::MalformedGpt(format!("missing u32 at offset {offset}"))
    })?;
    Ok(u32::from_le_bytes(value.try_into().expect("four bytes")))
}

fn le_u64(bytes: &[u8], offset: usize) -> Result<u64, RockUsbProtocolError> {
    let value = bytes.get(offset..offset + 8).ok_or_else(|| {
        RockUsbProtocolError::MalformedGpt(format!("missing u64 at offset {offset}"))
    })?;
    Ok(u64::from_le_bytes(value.try_into().expect("eight bytes")))
}

fn ceil_div(numerator: usize, denominator: usize) -> usize {
    numerator / denominator + usize::from(numerator % denominator != 0)
}

/// IEEE CRC-32 used by GPT headers and entry arrays.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RockUsbProtocolError {
    Transport(String),
    AddressOutOfRange { begin_sector: u64, sectors: u64 },
    TransferTooLarge { sectors: u64 },
    EmptyWrite,
    CswSignature([u8; 4]),
    CswTag { expected: u32, observed: u32 },
    CommandFailed { status: u8, residue: u32 },
    MalformedResponse(String),
    MalformedGpt(String),
}

impl fmt::Display for RockUsbProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(detail) => write!(f, "bulk transport: {detail}"),
            Self::AddressOutOfRange {
                begin_sector,
                sectors,
            } => write!(
                f,
                "sector range {begin_sector}+{sectors} exceeds the 32-bit RockUSB address"
            ),
            Self::TransferTooLarge { sectors } => {
                write!(f, "{sectors} sectors do not fit in host memory")
            }
            Self::EmptyWrite => f.write_str("WRITE_LBA payload is empty"),
            Self::CswSignature(observed) => {
                write!(f, "CSW signature is {:02x?}, expected USBS", observed)
            }
            Self::CswTag { expected, observed } => {
                write!(
                    f,
                    "CSW tag {observed:#010x} does not match CBW tag {expected:#010x}"
                )
            }
            Self::CommandFailed { status, residue } => {
                write!(f, "RockUSB command status {status} with residue {residue}")
            }
            Self::MalformedResponse(detail) => write!(f, "malformed RockUSB response: {detail}"),
            Self::MalformedGpt(detail) => write!(f, "malformed GPT: {detail}"),
        }
    }
}

impl std::error::Error for RockUsbProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    #[derive(Debug, Default)]
    struct ScriptedIo {
        writes: RefCell<Vec<Vec<u8>>>,
        reads: VecDeque<Vec<u8>>,
    }

    impl ScriptedIo {
        fn with_reads(reads: Vec<Vec<u8>>) -> Self {
            Self {
                writes: RefCell::new(Vec::new()),
                reads: reads.into(),
            }
        }
    }

    impl RockUsbBulkIo for ScriptedIo {
        fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.writes.borrow_mut().push(bytes.to_vec());
            Ok(())
        }

        fn read_some(&mut self, bytes: &mut [u8]) -> Result<usize, String> {
            let next = self.reads.pop_front().ok_or("no scripted read")?;
            if next.len() > bytes.len() {
                return Err(format!(
                    "script has {}, caller capacity is {}",
                    next.len(),
                    bytes.len()
                ));
            }
            bytes[..next.len()].copy_from_slice(&next);
            Ok(next.len())
        }

        fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), String> {
            let next = self.reads.pop_front().ok_or("no scripted read")?;
            if next.len() != bytes.len() {
                return Err(format!(
                    "script has {}, caller wants {}",
                    next.len(),
                    bytes.len()
                ));
            }
            bytes.copy_from_slice(&next);
            Ok(())
        }
    }

    fn csw(tag: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; CSW_BYTES];
        bytes[0..4].copy_from_slice(&CSW_SIGNATURE);
        bytes[4..8].copy_from_slice(&tag.to_le_bytes());
        bytes
    }

    #[test]
    fn read_lba_emits_the_pinned_rockusb_cbw_byte_for_byte() {
        let mut data = vec![0u8; LOGICAL_BLOCK_BYTES];
        data[0..8].copy_from_slice(b"EFI PART");
        let mut io = ScriptedIo::with_reads(vec![data.clone(), csw(0x1122_3344)]);
        let mut protocol = RockUsbProtocol::new(&mut io, 0x1122_3344);
        assert_eq!(protocol.read_lba(1, 1).unwrap(), data);
        let writes = io.writes.borrow();
        assert_eq!(writes.len(), 1);
        let cbw = &writes[0];
        assert_eq!(&cbw[0..4], b"USBC");
        assert_eq!(&cbw[4..8], &0x1122_3344u32.to_le_bytes());
        assert_eq!(&cbw[8..12], &512u32.to_le_bytes());
        assert_eq!(cbw[12], 0x80);
        assert_eq!(cbw[14], 10);
        assert_eq!(cbw[15], READ_LBA);
        assert_eq!(&cbw[17..21], &1u32.to_be_bytes());
        assert_eq!(&cbw[22..24], &1u16.to_be_bytes());
    }

    #[test]
    fn write_lba_uses_the_pinned_out_cbw_and_zero_pads_only_the_final_sector() {
        let payload = vec![0x5au8; LOGICAL_BLOCK_BYTES + 1];
        let mut io = ScriptedIo::with_reads(vec![csw(0x2233_4455)]);
        let mut protocol = RockUsbProtocol::new(&mut io, 0x2233_4455);
        let progress = protocol.write_lba(0x2000, &payload).unwrap();
        assert_eq!(
            progress,
            RockUsbTransferProgress {
                payload_bytes: 513,
                wire_sectors: 2,
                chunks: 1,
            }
        );
        let writes = io.writes.borrow();
        assert_eq!(writes.len(), 2);
        let cbw = &writes[0];
        assert_eq!(&cbw[0..4], b"USBC");
        assert_eq!(&cbw[8..12], &1024u32.to_le_bytes());
        assert_eq!(cbw[12], 0x00);
        assert_eq!(cbw[14], 10);
        assert_eq!(cbw[15], WRITE_LBA);
        assert_eq!(&cbw[17..21], &0x2000u32.to_be_bytes());
        assert_eq!(&cbw[22..24], &2u16.to_be_bytes());
        assert_eq!(&writes[1][..payload.len()], &payload);
        assert!(writes[1][payload.len()..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn write_lba_chunks_at_the_rockusb_128_sector_boundary() {
        let payload = vec![0xabu8; 129 * LOGICAL_BLOCK_BYTES];
        let mut io = ScriptedIo::with_reads(vec![csw(5), csw(6)]);
        let mut protocol = RockUsbProtocol::new(&mut io, 5);
        let progress = protocol.write_lba(9, &payload).unwrap();
        assert_eq!(progress.wire_sectors, 129);
        assert_eq!(progress.chunks, 2);
        let writes = io.writes.borrow();
        assert_eq!(writes.len(), 4);
        assert_eq!(&writes[0][17..21], &9u32.to_be_bytes());
        assert_eq!(&writes[0][22..24], &128u16.to_be_bytes());
        assert_eq!(&writes[2][17..21], &137u32.to_be_bytes());
        assert_eq!(&writes[2][22..24], &1u16.to_be_bytes());
    }

    #[test]
    fn device_reset_has_no_data_stage_and_requires_its_csw() {
        let mut io = ScriptedIo::with_reads(vec![csw(77)]);
        let mut protocol = RockUsbProtocol::new(&mut io, 77);
        protocol.reset_device().unwrap();
        let writes = io.writes.borrow();
        assert_eq!(writes.len(), 1);
        assert_eq!(&writes[0][8..12], &0u32.to_le_bytes());
        assert_eq!(writes[0][12], 0x00);
        assert_eq!(writes[0][14], 6);
        assert_eq!(writes[0][15], DEVICE_RESET);
    }

    #[test]
    fn an_empty_write_is_refused_before_a_cbw() {
        let mut io = ScriptedIo::default();
        let mut protocol = RockUsbProtocol::new(&mut io, 1);
        assert_eq!(protocol.write_lba(0, &[]), Err(RockUsbProtocolError::EmptyWrite));
        assert!(io.writes.borrow().is_empty());
    }

    #[test]
    fn a_mismatched_csw_tag_is_never_accepted() {
        let mut io = ScriptedIo::with_reads(vec![csw(9)]);
        let mut protocol = RockUsbProtocol::new(&mut io, 8);
        assert_eq!(
            protocol.test_unit_ready(),
            Err(RockUsbProtocolError::CswTag {
                expected: 8,
                observed: 9
            })
        );
    }

    #[test]
    fn capacity_uses_the_read_flash_info_wire_value() {
        let mut info = vec![0u8; 11];
        info[0..4].copy_from_slice(&31_250_000u32.to_le_bytes());
        let mut io = ScriptedIo::with_reads(vec![info, csw(7)]);
        let mut protocol = RockUsbProtocol::new(&mut io, 7);
        assert_eq!(protocol.read_capacity_sectors().unwrap(), 31_250_000);
        assert_eq!(io.writes.borrow()[0][15], READ_FLASH_INFO);
    }

    #[test]
    fn capacity_accepts_the_reference_implementation_padded_flash_info() {
        let mut info = vec![0u8; LOGICAL_BLOCK_BYTES];
        info[0..4].copy_from_slice(&31_250_000u32.to_le_bytes());
        let mut io = ScriptedIo::with_reads(vec![info, csw(17)]);
        let mut protocol = RockUsbProtocol::new(&mut io, 17);
        assert_eq!(protocol.read_capacity_sectors().unwrap(), 31_250_000);
        assert_eq!(&io.writes.borrow()[0][8..12], &11u32.to_le_bytes());
    }

    #[test]
    fn gpt_crc_is_not_treated_as_optional() {
        let mut block = vec![0u8; LOGICAL_BLOCK_BYTES];
        block[0..8].copy_from_slice(b"EFI PART");
        block[12..16].copy_from_slice(&92u32.to_le_bytes());
        block[72..80].copy_from_slice(&2u64.to_le_bytes());
        block[80..84].copy_from_slice(&128u32.to_le_bytes());
        block[84..88].copy_from_slice(&128u32.to_le_bytes());
        block[16..20].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        assert!(matches!(
            GptHeader::parse(&block),
            Err(RockUsbProtocolError::MalformedGpt(_))
        ));
    }

    #[test]
    fn a_valid_gpt_projects_arkforge_partition_semantics() {
        let mut entries = vec![0u8; 128 * 128];
        write_entry(&mut entries[0..128], 0x2000, 0x3fff, "uboot");
        write_entry(&mut entries[128..256], 0x4000, 0x7fff, "userdata");

        let mut block = vec![0u8; LOGICAL_BLOCK_BYTES];
        block[0..8].copy_from_slice(b"EFI PART");
        block[12..16].copy_from_slice(&92u32.to_le_bytes());
        block[72..80].copy_from_slice(&2u64.to_le_bytes());
        block[80..84].copy_from_slice(&128u32.to_le_bytes());
        block[84..88].copy_from_slice(&128u32.to_le_bytes());
        block[88..92].copy_from_slice(&crc32(&entries).to_le_bytes());
        let header_crc = crc32(&block[..92]);
        block[16..20].copy_from_slice(&header_crc.to_le_bytes());

        let header = GptHeader::parse(&block).unwrap();
        let table = parse_partition_entries(&header, &entries).unwrap();
        assert_eq!(table.device, DEVICE_STRING);
        assert_eq!(table.entries.len(), 2);
        assert_eq!(table.entries[0].index, 0);
        assert_eq!(table.entries[0].name, "uboot");
        assert_eq!(table.entries[0].offset_sectors, 0x2000);
        assert_eq!(table.entries[0].size_sectors, Some(0x2000));
        assert_eq!(table.entries[1].name, "userdata");
        assert_eq!(table.entries[1].size_sectors, None);
    }

    fn write_entry(entry: &mut [u8], first_lba: u64, last_lba: u64, name: &str) {
        entry[0] = 1; // a non-zero type GUID marks the entry as used
        entry[32..40].copy_from_slice(&first_lba.to_le_bytes());
        entry[40..48].copy_from_slice(&last_lba.to_le_bytes());
        for (slot, unit) in entry[56..128].chunks_exact_mut(2).zip(name.encode_utf16()) {
            slot.copy_from_slice(&unit.to_le_bytes());
        }
    }
}
