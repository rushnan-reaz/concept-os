// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Streaming opcode decoder for the COPY/ADD/END v1 algorithm.
//!
//! The decoder is a **pull-based state machine**.  The caller feeds small
//! buffers of opcode bytes and receives back an action describing what to do
//! next (copy from base, add literal data, or stop).  The decoder itself
//! performs **no I/O and allocates no memory** — it is designed for extremely
//! SRAM-constrained `no_std` environments.
//!
//! # Typical usage (device-side)
//!
//! ```ignore
//! let mut decoder = Decoder::new(delta_header.patch_payload_size, base_size, delta_header.target_size);
//! let mut opcode_buf = [0u8; MAX_OPCODE_HEADER];
//! loop {
//!     let n = uart_read(&mut opcode_buf[..]);
//!     let (action, consumed) = decoder.next_action(&opcode_buf[..n])?;
//!     match action {
//!         DecoderAction::Copy { src_offset, len } => { /* read base flash, write target */ }
//!         DecoderAction::Add { len } => { /* read `len` bytes from UART, write target */ }
//!         DecoderAction::Done => break,
//!     }
//! }
//! ```

use crate::error::DecodeError;
use crate::format::{OP_ADD, OP_COPY, OP_END};

// ---------------------------------------------------------------------------
//  Public types
// ---------------------------------------------------------------------------

/// An action that the caller must perform after decoding one opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderAction {
    /// Copy `len` bytes from the base CBF starting at byte offset `src_offset`
    /// into the reconstructed CBF at the current write position.
    Copy { src_offset: u32, len: u32 },

    /// The next `len` bytes in the patch stream are literal data (the ADD
    /// payload).  The caller must read exactly `len` bytes from the patch
    /// source and write them to the reconstructed CBF.
    ///
    /// **Important:** these `len` bytes are *not* consumed by the decoder.
    /// The caller reads them externally, then resumes calling `next_action`.
    Add { len: u32 },

    /// The opcode stream is complete.  No further calls should be made.
    Done,
}

/// Streaming opcode decoder.
///
/// Tracks how many payload bytes have been consumed, the current write
/// position in the reconstructed image, and rejects streams that violate
/// size invariants (output overflow, COPY out of base bounds).
pub struct Decoder {
    /// Total declared patch payload size (from `DeltaHeader.patch_payload_size`).
    total_size: u32,
    /// Number of payload bytes consumed so far (opcode headers + ADD data).
    consumed: u32,
    /// Size of the base (old) CBF image — COPY offsets are validated against this.
    base_size: u32,
    /// Size of the target (reconstructed) CBF image — output overflow checked.
    target_size: u32,
    /// Current write position in the reconstructed image.
    write_pos: u32,
    /// Whether we have seen the END opcode.
    done: bool,
    /// Length of an `Add` action whose literal data the caller must still
    /// consume via [`advance_add`].  `Some` between an `Add` action and its
    /// matching `advance_add`; `None` otherwise.
    pending_add: Option<u32>,
}

impl Decoder {
    /// Create a new decoder.
    ///
    /// - `patch_payload_size`: total bytes in the opcode stream (from DeltaHeader).
    /// - `base_size`: size of the old/base CBF image (for COPY bounds checking).
    /// - `target_size`: size of the reconstructed CBF image (for overflow checking).
    pub fn new(patch_payload_size: u32, base_size: u32, target_size: u32) -> Self {
        Self {
            total_size: patch_payload_size,
            consumed: 0,
            base_size,
            target_size,
            write_pos: 0,
            done: false,
            pending_add: None,
        }
    }

    /// Returns the number of patch payload bytes consumed so far.
    pub fn bytes_consumed(&self) -> u32 {
        self.consumed
    }

    /// Returns the current write position in the reconstructed image.
    pub fn write_pos(&self) -> u32 {
        self.write_pos
    }

    /// Decode the next opcode from `buf`.
    ///
    /// On success, returns the action to perform and the number of bytes from
    /// `buf` that were consumed (the opcode header).  For `Add` actions, the
    /// literal data bytes are **not** in `buf` — the caller must read them
    /// separately and then add their count via [`advance_add`].
    ///
    /// # Errors
    ///
    /// - `BufferTooShort` — `buf` doesn't contain a complete opcode header.
    ///   Accumulate more bytes and retry.
    /// - `UnknownOpcode` — unrecognised tag byte.
    /// - `CopyOutOfBounds` — COPY would read past the end of the base image.
    /// - `OutputOverflow` — COPY or ADD would write past `target_size`.
    /// - `PrematureEnd` — END arrived before `patch_payload_size` was reached.
    /// - `MissingEnd` — all payload bytes consumed but no END was seen.
    pub fn next_action(&mut self, buf: &[u8]) -> Result<(DecoderAction, usize), DecodeError> {
        if self.done {
            return Ok((DecoderAction::Done, 0));
        }

        // The caller must consume the literal data of a preceding ADD (via
        // `advance_add`) before decoding the next opcode.
        if self.pending_add.is_some() {
            return Err(DecodeError::PendingAddData);
        }

        // All declared payload bytes have been consumed but no END opcode was
        // seen — the stream is truncated.  (A well-formed stream reaches END
        // with `consumed == total_size - 1` and sets `done`, so this only fires
        // on a missing/removed END.)
        if self.consumed >= self.total_size {
            return Err(DecodeError::MissingEnd);
        }

        if buf.is_empty() {
            return Err(DecodeError::BufferTooShort);
        }

        let tag = buf[0];
        match tag {
            OP_COPY => {
                // Need 9 bytes total: tag(1) + src_offset(4) + length(4)
                if buf.len() < 9 {
                    return Err(DecodeError::BufferTooShort);
                }
                let src_offset = u32_le(&buf[1..5]);
                let len = u32_le(&buf[5..9]);
                // Safety: COPY must not read past the base image
                if src_offset.saturating_add(len) > self.base_size {
                    return Err(DecodeError::CopyOutOfBounds);
                }
                // Safety: COPY must not write past the target image
                if self.write_pos.saturating_add(len) > self.target_size {
                    return Err(DecodeError::OutputOverflow);
                }
                // Safety: the 9-byte header must fit within the declared payload
                if self.consumed.saturating_add(9) > self.total_size {
                    return Err(DecodeError::OverlongStream);
                }
                self.consumed += 9;
                self.write_pos += len;
                Ok((DecoderAction::Copy { src_offset, len }, 9))
            }
            OP_ADD => {
                // Need 5 bytes for the header: tag(1) + length(4)
                if buf.len() < 5 {
                    return Err(DecodeError::BufferTooShort);
                }
                let len = u32_le(&buf[1..5]);
                // Safety: ADD must not write past the target image
                if self.write_pos.saturating_add(len) > self.target_size {
                    return Err(DecodeError::OutputOverflow);
                }
                // Safety: the 5-byte header plus `len` data bytes must fit
                // within the declared payload.
                if self
                    .consumed
                    .saturating_add(5)
                    .saturating_add(len)
                    > self.total_size
                {
                    return Err(DecodeError::OverlongStream);
                }
                // Consume only the 5-byte header here.
                // The caller must read `len` data bytes externally and call
                // `advance_add(len)` afterwards.
                self.consumed += 5;
                self.write_pos += len;
                self.pending_add = Some(len);
                Ok((DecoderAction::Add { len }, 5))
            }
            OP_END => {
                self.consumed += 1;
                // Verify we consumed exactly the declared payload size
                if self.consumed != self.total_size {
                    return Err(DecodeError::PrematureEnd);
                }
                self.done = true;
                Ok((DecoderAction::Done, 1))
            }
            other => Err(DecodeError::UnknownOpcode(other)),
        }
    }

    /// Advance the consumed counter after the caller has externally read
    /// `n` bytes of ADD literal data from the patch stream.
    ///
    /// Must be called exactly once after each `Add { len }` action, with
    /// `n == len`.
    ///
    /// # Errors
    ///
    /// - `AddLengthMismatch` — no `Add` is pending, or `n` does not equal the
    ///   pending `Add` length.  Guards against a corrupted length silently
    ///   desynchronising the stream (previously `n` was added unchecked and
    ///   wrapped in release builds).
    /// - `OverlongStream` — consuming `n` more bytes would exceed the declared
    ///   `patch_payload_size`.
    pub fn advance_add(&mut self, n: u32) -> Result<(), DecodeError> {
        match self.pending_add {
            Some(expected) if expected == n => {
                self.pending_add = None;
                if self.consumed.saturating_add(n) > self.total_size {
                    return Err(DecodeError::OverlongStream);
                }
                self.consumed += n;
                Ok(())
            }
            _ => Err(DecodeError::AddLengthMismatch),
        }
    }

    /// Check whether the decoder has finished (received END).
    pub fn is_done(&self) -> bool {
        self.done
    }
}

// ---------------------------------------------------------------------------
//  Little-endian helpers (duplicated from format.rs to avoid coupling)
// ---------------------------------------------------------------------------

#[inline]
fn u32_le(b: &[u8]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}

// ---------------------------------------------------------------------------
//  Report generation (std only)
// ---------------------------------------------------------------------------

/// Decode an entire patch payload, reconstruct the output, and write a report
/// of every opcode to a file.
///
/// This is a host-side diagnostic tool — it walks the full opcode stream,
/// actually applies COPY/ADD operations against `old` to produce the
/// reconstructed image, logs each action, and returns the reconstructed bytes.
///
/// `target_size` is the expected size of the reconstructed image (from
/// `DeltaHeader.target_size`).  Pass `u32::MAX` to disable overflow checking.
#[cfg(feature = "std")]
pub fn decode_to_report(
    patch: &[u8],
    old: &[u8],
    target_size: u32,
    path: &std::path::Path,
) -> Result<Vec<u8>, DecodeError> {
    use std::io::Write;

    let file = std::fs::File::create(path)
        .expect(&std::format!("cannot create report file: {}", path.display()));
    let mut w = std::io::BufWriter::new(file);

    let _ = writeln!(w, "=== Delta Decoder Report ===");
    let _ = writeln!(w, "  patch payload size: {} bytes", patch.len());
    let _ = writeln!(w, "  base CBF size:      {} bytes", old.len());
    let _ = writeln!(w, "  target size:        {} bytes", target_size);
    let _ = writeln!(w);

    let mut decoder = Decoder::new(patch.len() as u32, old.len() as u32, target_size);
    let mut patch_pos: usize = 0;
    let mut opcode_num: usize = 0;
    let mut output: Vec<u8> = Vec::new();

    loop {
        let write_pos = decoder.write_pos();
        let (action, consumed) = decoder.next_action(&patch[patch_pos..])?;
        patch_pos += consumed;

        match action {
            DecoderAction::Copy { src_offset, len } => {
                opcode_num += 1;
                let start = src_offset as usize;
                let end = start + len as usize;
                let _ = writeln!(
                    w,
                    "[opcode #{:>3}] COPY  src_offset={:<6} len={:<6} base[{}..{}] -> out[{}..{}]",
                    opcode_num,
                    src_offset,
                    len,
                    start,
                    end,
                    write_pos,
                    write_pos + len
                );
                // Reconstruct: copy from base
                output.extend_from_slice(&old[start..end]);
            }
            DecoderAction::Add { len } => {
                opcode_num += 1;
                let data_start = patch_pos;
                let data_end = patch_pos + len as usize;
                let _ = writeln!(
                    w,
                    "[opcode #{:>3}] ADD   len={:<6} patch[{}..{}] -> out[{}..{}]",
                    opcode_num,
                    len,
                    data_start,
                    data_end,
                    write_pos,
                    write_pos + len
                );
                // Reconstruct: copy literal data from patch stream
                output.extend_from_slice(&patch[data_start..data_end]);
                decoder.advance_add(len)?;
                patch_pos += len as usize;
            }
            DecoderAction::Done => {
                opcode_num += 1;
                let _ = writeln!(w, "[opcode #{:>3}] END", opcode_num);
                let _ = writeln!(w);
                let _ = writeln!(
                    w,
                    "=== Summary: {} opcodes, {} bytes consumed, {} bytes reconstructed ===",
                    opcode_num,
                    patch_pos,
                    write_pos
                );
                let crc = crate::format::crc32b(&output);
                let _ = writeln!(w, "  reconstructed CRC32: 0x{:08X}", crc);
                break;
            }
        }
    }

    let _ = w.flush();
    Ok(output)
}

