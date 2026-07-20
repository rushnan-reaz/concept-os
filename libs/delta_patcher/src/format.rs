// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Delta CBF wire-format constants and structures.
//!
//! This module defines the Delta Header layout and opcode tags shared between
//! the host-side encoder and the device-side decoder.  Everything here is
//! `no_std`-compatible.

// ---------------------------------------------------------------------------
//  Magic & version constants
// ---------------------------------------------------------------------------

/// Delta Header magic bytes: `'D','E','L','T'` (0x544C4544 little-endian).
pub const DELTA_MAGIC: [u8; 4] = [b'D', b'E', b'L', b'T'];

/// Size of the Delta Header in bytes (fixed, multiple of 4).
pub const DELTA_HEADER_SIZE: usize = 32;

/// The only format version understood by v1 decoders.
pub const DELTA_FORMAT_V1: u16 = 1;

/// Algorithm ID for the COPY/ADD/END v1 opcode set.
pub const ALGO_COPY_ADD_V1: u16 = 0x0001;

// ---------------------------------------------------------------------------
//  Opcode tags
// ---------------------------------------------------------------------------

/// COPY opcode tag.  Total wire size: 9 bytes (tag + src_offset:u32 + len:u32).
pub const OP_COPY: u8 = 0x01;

/// ADD opcode tag.  Total wire size: 5 + N bytes (tag + len:u32 + data[N]).
pub const OP_ADD: u8 = 0x02;

/// END sentinel tag.  Total wire size: 1 byte.
pub const OP_END: u8 = 0x03;

/// Maximum opcode header size in bytes (COPY = 1 + 4 + 4 = 9).
pub const MAX_OPCODE_HEADER: usize = 9;

// ---------------------------------------------------------------------------
//  Delta Header
// ---------------------------------------------------------------------------

/// The 32-byte Delta Header placed immediately after the standard CBF header
/// chain (at the position where the payload would normally begin).
///
/// All multi-byte fields are little-endian on the wire.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeltaHeader {
    pub magic: [u8; 4],
    pub format_version: u16,
    pub algorithm_id: u16,
    pub base_component_id: u16,
    pub base_component_version: u32,
    pub reserved: u16,
    pub base_crc32: u32,
    pub reconstructed_crc32: u32,
    pub patch_payload_size: u32,
    /// Total size of the reconstructed (target) CBF image in bytes.
    /// The device uses this to allocate the scratch flash block.
    pub target_size: u32,
}

impl DeltaHeader {
    /// Parse a `DeltaHeader` from a 32-byte little-endian buffer.
    ///
    /// Returns `None` if `buf.len() < DELTA_HEADER_SIZE`.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < DELTA_HEADER_SIZE {
            return None;
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&buf[0..4]);
        Some(Self {
            magic,
            format_version: u16_le(&buf[4..6]),
            algorithm_id: u16_le(&buf[6..8]),
            base_component_id: u16_le(&buf[8..10]),
            base_component_version: u32_le(&buf[10..14]),
            reserved: u16_le(&buf[14..16]),
            base_crc32: u32_le(&buf[16..20]),
            reconstructed_crc32: u32_le(&buf[20..24]),
            patch_payload_size: u32_le(&buf[24..28]),
            target_size: u32_le(&buf[28..32]),
        })
    }

    /// Serialize this header into a 32-byte little-endian buffer.
    pub fn to_bytes(&self) -> [u8; DELTA_HEADER_SIZE] {
        let mut out = [0u8; DELTA_HEADER_SIZE];
        out[0..4].copy_from_slice(&self.magic);
        out[4..6].copy_from_slice(&self.format_version.to_le_bytes());
        out[6..8].copy_from_slice(&self.algorithm_id.to_le_bytes());
        out[8..10].copy_from_slice(&self.base_component_id.to_le_bytes());
        out[10..14].copy_from_slice(&self.base_component_version.to_le_bytes());
        out[14..16].copy_from_slice(&self.reserved.to_le_bytes());
        out[16..20].copy_from_slice(&self.base_crc32.to_le_bytes());
        out[20..24].copy_from_slice(&self.reconstructed_crc32.to_le_bytes());
        out[24..28].copy_from_slice(&self.patch_payload_size.to_le_bytes());
        out[28..32].copy_from_slice(&self.target_size.to_le_bytes());
        out
    }

    /// Validate the structural fields of this header (magic, version, algo).
    /// Does NOT verify CRC values — those require access to the base/new CBFs.
    pub fn validate(&self) -> bool {
        self.magic == DELTA_MAGIC
            && self.format_version == DELTA_FORMAT_V1
            && self.algorithm_id == ALGO_COPY_ADD_V1
            && self.reserved == 0
    }
}

// ---------------------------------------------------------------------------
//  CRC-32b  (ISO 3309 / ITU-T V.42, same polynomial as zlib / Ethernet)
// ---------------------------------------------------------------------------

/// Compute CRC-32b over a byte slice.
///
/// Uses the standard reflected polynomial `0xEDB88320`.  This is a simple
/// byte-at-a-time implementation suitable for both `std` and `no_std`
/// environments.
pub fn crc32b(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Incremental CRC-32b state for streaming computation.
pub struct Crc32State {
    crc: u32,
}

impl Crc32State {
    /// Create a new CRC-32b accumulator.
    pub fn new() -> Self {
        Self { crc: 0xFFFF_FFFF }
    }

    /// Feed a chunk of bytes.
    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.crc ^= byte as u32;
            for _ in 0..8 {
                if self.crc & 1 != 0 {
                    self.crc = (self.crc >> 1) ^ 0xEDB8_8320;
                } else {
                    self.crc >>= 1;
                }
            }
        }
    }

    /// Finalize and return the CRC-32b value.
    pub fn finalize(self) -> u32 {
        !self.crc
    }
}

// ---------------------------------------------------------------------------
//  Little-endian helpers
// ---------------------------------------------------------------------------

#[inline]
fn u16_le(b: &[u8]) -> u16 {
    (b[0] as u16) | ((b[1] as u16) << 8)
}

#[inline]
fn u32_le(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}
