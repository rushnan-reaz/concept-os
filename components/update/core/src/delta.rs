// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Device-side delta update path.
//!
//! Reached from `update::component_add_update` when the fixed header has the
//! `IS_DELTA` flag set. Flow (see `documentation/ConceptOSDeltaBinaryFormat.md`):
//!
//! 1. Pull + validate the 32-byte Delta Header (command `0xB0`).
//! 2. Locate the base component in flash by id + version.
//! 3. Allocate a scratch block and drive the decoder to reconstruct the
//!    pristine `new.hbf` into it (COPY reads base flash, ADD pulls literals).
//! 4. Verify the reconstructed CRC-32b — the correctness gate.
//! 5. Install from scratch via the existing relocate/validate/load path.
//! 6. Free scratch; send Success; `load_component`.

use crate::consts::PACKET_BUFFER_SIZE;
use crate::messages::*;
use crate::utils::{channel_ask, channel_write_single, wrap_hbf_error, FlashReader};
use delta_patcher::format::DeltaHeader;
use delta_patcher::{Crc32State, Decoder, DecoderAction, DELTA_HEADER_SIZE, MAX_OPCODE_HEADER};
use storage_api::{Storage, StorageError};
use uart_channel_api::UartChannel;
use userlib::flash::BlockType;
use userlib::sys_log;

/// Entry point for a delta component update. The caller has already pulled and
/// validated the outer fixed header and confirmed `IS_DELTA` is set.
pub fn component_add_delta_update(channel: &mut UartChannel) -> Result<(), MessageError> {
    // One-time GPIOC setup for the phase markers (no-op unless `profiling`).
    // Runs before any marker is raised, so its cost is outside every phase.
    crate::markers::markers_init();
    use crate::markers::Marker;

    // PC0: pull + validate the delta header.
    let header = {
        let _m = Marker::new(0);
        pull_delta_header(channel)?
    };
    sys_log!(
        "[UPDATE][delta] header ok: base id={} ver={} patch={}B target={}B",
        header.base_component_id,
        header.base_component_version,
        header.patch_payload_size,
        header.target_size
    );

    // PC1: locate the base component in flash by id + version.
    let (base_base, base_size) = {
        let _m = Marker::new(1);
        find_base(&header)?
    };
    sys_log!("[UPDATE][delta] base found @ {:#010x} ({}B)", base_base, base_size);

    // PC2: masked base-CRC early check (Phase-2). Distinguishes "wrong base"
    // from "corrupt patch" and fails fast; correctness does not depend on it
    // (the reconstructed-CRC gate below is the authoritative check).
    {
        let _m = Marker::new(2);
        verify_masked_base_crc(base_base, base_size, header.base_crc32)?;
    }
    sys_log!("[UPDATE][delta] masked base CRC ok");

    // Allocate scratch for the reconstructed image (flash only, ram_size = 0).
    let scratch = {
        let mut storage = Storage::new();
        storage
            .allocate_component(header.target_size, 0)
            .map_err(map_alloc_err)?
    };

    // PC3: reconstruct pristine new.hbf into scratch and verify its CRC-32b
    // (the phase that dominates the update; do not toggle inside its loop).
    let recon = {
        let _m = Marker::new(3);
        reconstruct(channel, &header, base_base, base_size, scratch.flash_base_address)
    };
    if let Err(e) = recon {
        free_block(scratch.flash_base_address);
        return Err(e);
    }
    sys_log!("[UPDATE][delta] reconstruction verified");

    // PC4: install from scratch (reuses the proven relocate/validate/load path).
    let install = {
        let _m = Marker::new(4);
        crate::update::install_reconstructed_from_scratch(
            channel,
            scratch.flash_base_address,
            scratch.flash_size,
        )
    };
    // Scratch is no longer needed; free it before load_component (task switch).
    free_block(scratch.flash_base_address);
    let final_base = install?;

    // Success must be sent BEFORE load_component, which can preempt this task.
    channel_write_single(channel, ComponentUpdateResponse::Success as u8)?;
    sys_log!("[UPDATE][delta] success, loading component @ {:#010x}", final_base);
    if !userlib::kipc::load_component(final_base) {
        sys_log!("[UPDATE][delta] load_component failed");
    }
    Ok(())
}

/// Pull the 32-byte Delta Header (`SendDeltaHeader = 0xB0`), validate its CRC-8
/// framing, parse it, and validate its structural fields.
fn pull_delta_header(channel: &mut UartChannel) -> Result<DeltaHeader, MessageError> {
    let mut buf: [u8; DELTA_HEADER_SIZE + 1] = [0x00; DELTA_HEADER_SIZE + 1];
    channel_ask(channel, ComponentUpdateCommand::SendDeltaHeader as u8, &mut buf)?;
    RawPacket::validate(&buf)?;
    let header =
        DeltaHeader::from_bytes(&buf[0..DELTA_HEADER_SIZE]).ok_or(MessageError::InvalidSize)?;
    if !header.validate() {
        return Err(MessageError::FailedHBFValidation);
    }
    Ok(header)
}

/// Scan storage blocks for the COMPONENT whose HBF id + version match the delta
/// header's base. Returns `(base_flash_base, base_total_size)`.
fn find_base(header: &DeltaHeader) -> Result<(u32, u32), MessageError> {
    let storage = Storage::new();
    let status = storage.report_status().map_err(|_| MessageError::FlashError)?;
    for i in 0..status.blocks {
        let block = storage.get_nth_block(i).map_err(|_| MessageError::FlashError)?;
        if block.block_type != BlockType::COMPONENT {
            continue;
        }
        let mut buf: [u8; hbf_lite::HBF_HEADER_MIN_SIZE] = [0x00; hbf_lite::HBF_HEADER_MIN_SIZE];
        if storage
            .read_stream(block.block_base_address, 0, &mut buf)
            .is_err()
        {
            continue;
        }
        let reader = hbf_lite::BufferReaderImpl::from(&buf);
        let hbf = match hbf_lite::HbfFile::from_reader(&reader) {
            Ok(h) => h,
            Err(_) => continue,
        };
        let hb = match hbf.header_base() {
            Ok(h) => h,
            Err(_) => continue,
        };
        if hb.component_id() == header.base_component_id
            && hb.component_version() == header.base_component_version
        {
            return Ok((block.block_base_address, hb.total_size()));
        }
    }
    // No matching base installed.
    Err(MessageError::DeltaBaseNotFound)
}

/// Maximum relocation count the on-device masked-CRC check will handle. Beyond
/// this the check is skipped (it is only an optimization; the reconstructed-CRC
/// gate still guarantees correctness). Kept small to bound stack use in the
/// RAM-constrained update component (64 * 4 = 256 bytes).
const MAX_BASE_RELOCS: usize = 64;

/// Phase-2 masked base-CRC verification.
///
/// Recompute the CRC-32b over the base HBF in flash with every relocation-site
/// word and the 4-byte trailer **zeroed** — the exact `forbidden` masking
/// `delta_gen` applied to the pristine base — and compare to the delta header's
/// `base_crc32`. Zeroing is idempotent, so the device does not need to sort or
/// merge the ranges: it just zeroes each forbidden 4-byte window.
///
/// Skipped silently if the base has more than `MAX_BASE_RELOCS` relocations.
fn verify_masked_base_crc(
    base_base: u32,
    base_size: u32,
    expected: u32,
) -> Result<(), MessageError> {
    // Parse the base header from flash to read its own relocation table.
    let (total, checksum_offset, relocs, num_relocs) = {
        let reader = FlashReader::from(base_base, base_size);
        let hbf = wrap_hbf_error(hbf_lite::HbfFile::from_reader(&reader))?;
        let hb = wrap_hbf_error(hbf.header_base())?;
        let num = hb.num_relocations() as usize;
        if num > MAX_BASE_RELOCS {
            sys_log!("[UPDATE][delta] base has {} relocs (> {}), skipping masked CRC", num, MAX_BASE_RELOCS);
            return Ok(());
        }
        let total = hb.total_size();
        let checksum_offset = wrap_hbf_error(hbf.checksum_offset())?;
        // Snapshot the reloc file offsets once (avoid re-reading flash per chunk).
        let mut relocs = [0u32; MAX_BASE_RELOCS];
        for i in 0..num {
            let v = wrap_hbf_error(hbf.relocation_nth(i as u32))?.value();
            relocs[i] = v & 0x00FF_FFFF;
        }
        (total, checksum_offset, relocs, num)
    };

    let mut crc = Crc32State::new();
    let mut storage = Storage::new();
    let mut tmp: [u8; PACKET_BUFFER_SIZE] = [0x00; PACKET_BUFFER_SIZE];
    let mut off: u32 = 0;
    while off < total {
        let n = core::cmp::min(PACKET_BUFFER_SIZE as u32, total - off) as usize;
        storage
            .read_stream(base_base, off, &mut tmp[0..n])
            .map_err(|_| MessageError::FlashError)?;
        // Zero the 4-byte trailer window and every relocation-site window that
        // overlaps this chunk.
        zero_window(&mut tmp[0..n], off, checksum_offset, 4);
        for &fo in &relocs[0..num_relocs] {
            zero_window(&mut tmp[0..n], off, fo, 4);
        }
        crc.update(&tmp[0..n]);
        off += n as u32;
    }

    if crc.finalize() != expected {
        return Err(MessageError::DeltaBaseCrcMismatch);
    }
    Ok(())
}

/// Zero the bytes of `chunk` (which starts at file offset `chunk_off`) that fall
/// within the file window `[win_start, win_start + win_len)`.
fn zero_window(chunk: &mut [u8], chunk_off: u32, win_start: u32, win_len: u32) {
    let chunk_end = chunk_off + chunk.len() as u32;
    let win_end = win_start + win_len;
    let lo = core::cmp::max(chunk_off, win_start);
    let hi = core::cmp::min(chunk_end, win_end);
    if lo < hi {
        for i in lo..hi {
            chunk[(i - chunk_off) as usize] = 0;
        }
    }
}

/// Drive the decoder to reconstruct the pristine `new.hbf` into `scratch_base`,
/// then verify the running CRC-32b against `header.reconstructed_crc32`.
fn reconstruct(
    channel: &mut UartChannel,
    header: &DeltaHeader,
    base_base: u32,
    base_size: u32,
    scratch_base: u32,
) -> Result<(), MessageError> {
    let mut reader = PatchReader::new(channel, header.patch_payload_size as usize);
    let mut decoder = Decoder::new(header.patch_payload_size, base_size, header.target_size);
    let mut crc = Crc32State::new();
    let mut storage = Storage::new();
    let mut cursor: u32 = 0;
    let mut tmp: [u8; PACKET_BUFFER_SIZE] = [0x00; PACKET_BUFFER_SIZE];

    loop {
        // Ensure a full opcode header is buffered (or the tail END byte).
        reader.ensure(MAX_OPCODE_HEADER)?;
        let (action, consumed) = decoder
            .next_action(reader.peek())
            .map_err(|_| MessageError::FailedHBFValidation)?;
        match action {
            DecoderAction::Copy { src_offset, len } => {
                reader.consume(consumed);
                let mut src = src_offset;
                let mut left = len;
                while left > 0 {
                    let n = core::cmp::min(PACKET_BUFFER_SIZE as u32, left) as usize;
                    storage
                        .read_stream(base_base, src, &mut tmp[0..n])
                        .map_err(|_| MessageError::FlashError)?;
                    storage
                        .write_stream(scratch_base, cursor, &tmp[0..n], false)
                        .map_err(|_| MessageError::FlashError)?;
                    crc.update(&tmp[0..n]);
                    cursor += n as u32;
                    src += n as u32;
                    left -= n as u32;
                }
            }
            DecoderAction::Add { len } => {
                reader.consume(consumed);
                let mut left = len;
                while left > 0 {
                    let n = core::cmp::min(PACKET_BUFFER_SIZE as u32, left) as usize;
                    reader.read_into(&mut tmp[0..n])?;
                    storage
                        .write_stream(scratch_base, cursor, &tmp[0..n], false)
                        .map_err(|_| MessageError::FlashError)?;
                    crc.update(&tmp[0..n]);
                    cursor += n as u32;
                    left -= n as u32;
                }
                decoder
                    .advance_add(len)
                    .map_err(|_| MessageError::FailedHBFValidation)?;
            }
            DecoderAction::Done => break,
        }
    }

    // Flush the final scratch writes.
    storage
        .write_stream(scratch_base, cursor, &[], true)
        .map_err(|_| MessageError::FlashError)?;

    // Correctness gate: the reconstructed image must match new.hbf exactly.
    if crc.finalize() != header.reconstructed_crc32 {
        sys_log!("[UPDATE][delta] reconstructed CRC mismatch");
        return Err(MessageError::DeltaReconstructMismatch);
    }
    Ok(())
}

fn map_alloc_err(e: StorageError) -> MessageError {
    match e {
        StorageError::OutOfFlash | StorageError::OutOfRam => MessageError::NotEnoughSpace,
        _ => MessageError::FlashError,
    }
}

fn free_block(base: u32) {
    Storage::new().deallocate_block(base).ok();
}

/// Buffers the patch opcode stream, pulling `SendNextFragment` (`0xA0`)
/// fragments from the channel on demand. The device drives the transfer: each
/// pull requests `min(PACKET_BUFFER_SIZE, remaining)` bytes, CRC-8 checked.
struct PatchReader<'a> {
    channel: &'a mut UartChannel,
    remaining: usize,
    buf: [u8; PACKET_BUFFER_SIZE * 2],
    start: usize,
    end: usize,
}

impl<'a> PatchReader<'a> {
    fn new(channel: &'a mut UartChannel, patch_size: usize) -> Self {
        Self {
            channel,
            remaining: patch_size,
            buf: [0x00; PACKET_BUFFER_SIZE * 2],
            start: 0,
            end: 0,
        }
    }

    fn available(&self) -> usize {
        self.end - self.start
    }

    fn peek(&self) -> &[u8] {
        &self.buf[self.start..self.end]
    }

    fn consume(&mut self, n: usize) {
        self.start += n;
    }

    /// Ensure at least `k` bytes are buffered, or the patch stream is exhausted.
    fn ensure(&mut self, k: usize) -> Result<(), MessageError> {
        while self.available() < k && self.remaining > 0 {
            self.pull_fragment()?;
        }
        Ok(())
    }

    /// Read exactly `dst.len()` bytes of buffered/streamed patch data.
    fn read_into(&mut self, dst: &mut [u8]) -> Result<(), MessageError> {
        let mut done = 0;
        while done < dst.len() {
            if self.available() == 0 {
                if self.remaining == 0 {
                    return Err(MessageError::FailedHBFValidation);
                }
                self.pull_fragment()?;
            }
            let take = core::cmp::min(self.available(), dst.len() - done);
            dst[done..done + take].copy_from_slice(&self.buf[self.start..self.start + take]);
            self.start += take;
            done += take;
        }
        Ok(())
    }

    fn pull_fragment(&mut self) -> Result<(), MessageError> {
        let n = core::cmp::min(PACKET_BUFFER_SIZE, self.remaining);
        if n == 0 {
            return Ok(());
        }
        // Compact any unconsumed bytes to the front to make room.
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        let mut frag: [u8; PACKET_BUFFER_SIZE + 1] = [0x00; PACKET_BUFFER_SIZE + 1];
        channel_ask(
            self.channel,
            ComponentUpdateCommand::SendNextFragment as u8,
            &mut frag[0..n + 1],
        )?;
        RawPacket::validate(&frag[0..n + 1])?;
        self.buf[self.end..self.end + n].copy_from_slice(&frag[0..n]);
        self.end += n;
        self.remaining -= n;
        Ok(())
    }
}
