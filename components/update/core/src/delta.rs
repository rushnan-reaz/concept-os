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
//! 3. Reconstruct the pristine `new.hbf` directly into a single, final flash
//!    block (COPY reads base flash, ADD pulls literals), relocating the
//!    payload in the same pass -- no separate scratch block. See
//!    `reconstruct()` for the three-stage breakdown (prefix / header-tail /
//!    payload).
//! 4. Verify the reconstructed CRC-32b (delta protocol gate) and the HBF's
//!    own XOR trailer checksum (kernel-load gate), same as the full-component
//!    install path.
//! 5. Send Success; `load_component`.

use crate::consts::PACKET_BUFFER_SIZE;
use crate::markers::Marker;
use crate::messages::*;
use crate::update::{ChecksumBuff, UpdateMethods, UpdateRelocator};
use crate::utils::{channel_ask, channel_write_single, wrap_hbf_error, FlashReader};
use delta_patcher::format::DeltaHeader;
use delta_patcher::{Crc32State, Decoder, DecoderAction, DELTA_HEADER_SIZE, MAX_OPCODE_HEADER};
use hbf_lite::{BufferReaderImpl, HbfFile};
use relocator::Relocator;
use storage_api::{Storage, StorageError};
use uart_channel_api::UartChannel;
use userlib::flash::BlockType;
use userlib::sys_log;

use crate::consts::{BUFF_SIZE, LINKED_FLASH_BASE, LINKED_SRAM_BASE, RELOC_BUFF_SIZE};

/// Entry point for a delta component update. The caller has already pulled and
/// validated the outer fixed header and confirmed `IS_DELTA` is set.
pub fn component_add_delta_update(channel: &mut UartChannel) -> Result<(), MessageError> {
    // GPIOC marker setup now happens once at component startup (main.rs),
    // not here. It used to run as the very first statement of this
    // function, immediately before `Marker::new(0)` -- close enough (a
    // handful of instructions) that the low-then-high transition could
    // land inside a single 2 MHz sample period and disappear, merging
    // whatever the pin was doing before init (floating, from board reset)
    // invisibly into the start of the phase-0 (header_pull) pulse. Moving
    // init to component startup gives PC5 a long, stable low period before
    // phase 0 ever fires, so that transition is unambiguous.

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
    let base_check = {
        let _m = Marker::new(2);
        verify_masked_base_crc(base_base, base_size, header.base_crc32)?
    };
    match base_check {
        BaseCheckOutcome::Verified => {
            sys_log!("[UPDATE][delta] masked base CRC verified");
        }
        BaseCheckOutcome::SkippedTooManyRelocs => {
            sys_log!(
                "[UPDATE][delta] masked base CRC skipped (>{} relocs); correctness deferred to reconstructed-CRC",
                MAX_BASE_RELOCS
            );
        }
    }

    // PC3+PC5+PC4 combined: reconstruct directly into a single final block,
    // relocating the payload as it's written (no scratch, no second copy
    // pass). See `reconstruct()` for the allocation point (fires partway
    // through, once the prefix header is known).
    let final_base = {
        let _m = Marker::new(3);
        reconstruct(channel, &header, base_base, base_size)?
    };
    sys_log!("[UPDATE][delta] reconstruction + install verified @ {:#010x}", final_base);

    // Success must be sent BEFORE load_component, which can preempt this task.
    channel_write_single(channel, ComponentUpdateResponse::Success as u8)?;
    sys_log!("[UPDATE][delta] success, loading component @ {:#010x}", final_base);
    // PC4 (phase 4, revived): brackets only the load_component() call itself,
    // matching bthermo-performance's INSTALL marker exactly (see
    // components/update/core/src/update.rs on that branch: `markers::set(INSTALL)`
    // / `load_component(...)` / `markers::clear(INSTALL)` -- not the preceding
    // Success response). This is the direct INSTALL-equivalent for cross-branch
    // comparison.
    let loaded = {
        let _m = Marker::new(4);
        userlib::kipc::load_component(final_base)
    };
    if !loaded {
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

/// Outcome of the optional masked base-CRC check. `Verified` means the CRC ran
/// and matched the delta header's `base_crc32`. `SkippedTooManyRelocs` means the
/// base exceeded `MAX_BASE_RELOCS`, so the optimization was not run -- correctness
/// is then guaranteed unconditionally by the mandatory reconstructed-CRC gate.
/// A genuine mismatch is reported out-of-band as `Err(DeltaBaseCrcMismatch)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BaseCheckOutcome {
    Verified,
    SkippedTooManyRelocs,
}

/// Phase-2 masked base-CRC verification.
///
/// Recompute the CRC-32b over the base HBF in flash with every relocation-site
/// word and the 4-byte trailer **zeroed** -- the exact `forbidden` masking
/// `delta_gen` applied to the pristine base -- and compare to the delta header's
/// `base_crc32`. Zeroing is idempotent, so the device does not need to sort or
/// merge the ranges: it just zeroes each forbidden 4-byte window.
///
/// Returns [`BaseCheckOutcome::Verified`] when the CRC ran and matched, and
/// [`BaseCheckOutcome::SkippedTooManyRelocs`] when the base exceeds
/// `MAX_BASE_RELOCS` so the optimization could not run. The two are distinct so
/// a skipped check can never be mistaken for a verified base; correctness in the
/// skipped case rests on the mandatory reconstructed-CRC gate. A genuine
/// mismatch is `Err(DeltaBaseCrcMismatch)` (fail fast).
fn verify_masked_base_crc(
    base_base: u32,
    _base_size: u32,
    expected: u32,
) -> Result<BaseCheckOutcome, MessageError> {
    let storage = Storage::new();

    // Read the fixed header the SAME way `find_base` does -- one `read_stream`
    // into a zeroed stack buffer, parsed in-memory with `BufferReaderImpl`.
    // The previous per-field `FlashReader` path misread the header from flash
    // (garbage `num_relocations`/`total_size`), causing this check to silently
    // skip or fault. Every read here goes through `read_stream`, which the CRC
    // loop and `reconstruct` already use reliably.
    let mut hdr_buf = [0u8; hbf_lite::HBF_HEADER_MIN_SIZE];
    storage
        .read_stream(base_base, 0, &mut hdr_buf)
        .map_err(|_| MessageError::FlashError)?;
    let (total, checksum_offset, reloc_offset, num) = {
        let reader = hbf_lite::BufferReaderImpl::from(&hdr_buf);
        let hbf = wrap_hbf_error(hbf_lite::HbfFile::from_reader(&reader))?;
        let hb = wrap_hbf_error(hbf.header_base())?;
        (
            hb.total_size(),
            wrap_hbf_error(hbf.checksum_offset())?,
            hb.offset_relocation() as u32,
            hb.num_relocations() as usize,
        )
    };

    if num > MAX_BASE_RELOCS {
        sys_log!("[UPDATE][delta] base has {} relocs (> {}), skipping masked CRC", num, MAX_BASE_RELOCS);
        return Ok(BaseCheckOutcome::SkippedTooManyRelocs);
    }

    // Snapshot the relocation file-offsets, reading each 4-byte entry directly
    // via `read_stream` (no `FlashReader`). Table lives at `offset_relocation`,
    // entries are `RELOC_SIZE` apart; `value()` is the entry's first u32.
    let mut relocs = [0u32; MAX_BASE_RELOCS];
    for i in 0..num {
        let mut rbuf = [0u8; 4];
        let entry_off = reloc_offset + (i as u32) * (hbf_lite::RELOC_SIZE as u32);
        storage
            .read_stream(base_base, entry_off, &mut rbuf)
            .map_err(|_| MessageError::FlashError)?;
        relocs[i] = u32::from_le_bytes(rbuf) & 0x00FF_FFFF;
    }

    // Stream the base HBF, zeroing the trailer + relocation-site windows, CRC.
    let mut crc = Crc32State::new();
    let mut tmp: [u8; PACKET_BUFFER_SIZE] = [0x00; PACKET_BUFFER_SIZE];
    let mut off: u32 = 0;
    while off < total {
        let n = core::cmp::min(PACKET_BUFFER_SIZE as u32, total - off) as usize;
        storage
            .read_stream(base_base, off, &mut tmp[0..n])
            .map_err(|_| MessageError::FlashError)?;
        zero_window(&mut tmp[0..n], off, checksum_offset, 4);
        for &fo in &relocs[0..num] {
            zero_window(&mut tmp[0..n], off, fo, 4);
        }
        crc.update(&tmp[0..n]);
        off += n as u32;
    }

    if crc.finalize() != expected {
        return Err(MessageError::DeltaBaseCrcMismatch);
    }
    Ok(BaseCheckOutcome::Verified)
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

/// Size of the fixed-position prefix (`HbfHeaderBase` + `HbfHeaderMain`) that
/// carries everything needed to size the final block: `total_size()` (flash)
/// and `component_min_ram()` (RAM). Both structs are `#[repr(packed, C)]` with
/// no trailing padding, so this is exact. Buffered in RAM before any flash
/// allocation happens.
const PREFIX_SIZE: usize =
    core::mem::size_of::<hbf_lite::HbfHeaderBase>() + core::mem::size_of::<hbf_lite::HbfHeaderMain>();

/// Where `PatchReader` pulls its next raw fragment from. Passed as an
/// explicit, freshly-constructed argument to each call rather than stored in
/// `PatchReader` itself, so it can switch from the raw channel (before any
/// flash allocation exists) to `UpdateMethods` (after) without a persistent
/// borrow of `channel` blocking the later move into
/// `UpdateMethods::methods_for_requirements`.
enum FragSource<'s, 'um> {
    Channel(&'s mut UartChannel),
    Methods(&'s mut UpdateMethods<'um>),
}

impl<'s, 'um> FragSource<'s, 'um> {
    fn pull(&mut self, cmd: u8, buf: &mut [u8]) -> Result<(), MessageError> {
        match self {
            FragSource::Channel(c) => channel_ask(c, cmd, buf),
            FragSource::Methods(m) => m.channel_ask(cmd, buf),
        }
    }
}

/// A decoder action that was only partially consumed because a stage boundary
/// (prefix / header-tail / payload) was reached mid-action. Carried across
/// `next_chunk` calls so decoding resumes exactly where it left off, without
/// ever re-calling `Decoder::next_action` mid-action (which would desync the
/// stream).
enum PendingOp {
    None,
    Copy { src: u32, left: u32 },
    /// `total_len` is the action's original length, needed for the single
    /// `Decoder::advance_add` call once `left` reaches 0 -- see its own
    /// doc comment ("must be called exactly once ... with n == len").
    Add { total_len: u32, left: u32 },
}

/// Produce up to `max_len` bytes of the reconstructed stream (from base COPY
/// or wire ADD) into `tmp[0..n]`, continuing a partially-consumed action if
/// `pending` holds one, or pulling a fresh action otherwise. Updates the
/// delta-protocol CRC gate over every byte produced, regardless of stage.
/// Returns `n == 0` only at the true end of the stream (`DecoderAction::Done`).
fn next_chunk(
    reader: &mut PatchReader,
    decoder: &mut Decoder,
    pending: &mut PendingOp,
    source: &mut FragSource,
    base_storage: &Storage,
    base_base: u32,
    tmp: &mut [u8; PACKET_BUFFER_SIZE],
    max_len: usize,
    crc: &mut Crc32State,
) -> Result<usize, MessageError> {
    if matches!(pending, PendingOp::None) {
        reader.ensure(source, MAX_OPCODE_HEADER)?;
        let (action, consumed) = decoder
            .next_action(reader.peek())
            .map_err(|_| MessageError::FailedHBFValidation)?;
        reader.consume(consumed);
        match action {
            DecoderAction::Copy { src_offset, len } => {
                *pending = PendingOp::Copy { src: src_offset, left: len };
            }
            DecoderAction::Add { len } => {
                *pending = PendingOp::Add { total_len: len, left: len };
            }
            DecoderAction::Done => return Ok(0),
        }
    }
    match pending {
        PendingOp::Copy { src, left } => {
            let n = core::cmp::min(core::cmp::min(PACKET_BUFFER_SIZE, max_len) as u32, *left) as usize;
            base_storage
                .read_stream(base_base, *src, &mut tmp[0..n])
                .map_err(|_| MessageError::FlashError)?;
            crc.update(&tmp[0..n]);
            *src += n as u32;
            *left -= n as u32;
            if *left == 0 {
                *pending = PendingOp::None;
            }
            Ok(n)
        }
        PendingOp::Add { total_len, left } => {
            let n = core::cmp::min(core::cmp::min(PACKET_BUFFER_SIZE, max_len) as u32, *left) as usize;
            reader.read_into(source, &mut tmp[0..n])?;
            crc.update(&tmp[0..n]);
            *left -= n as u32;
            if *left == 0 {
                decoder
                    .advance_add(*total_len)
                    .map_err(|_| MessageError::FailedHBFValidation)?;
                *pending = PendingOp::None;
            }
            Ok(n)
        }
        PendingOp::None => Ok(0),
    }
}

/// Reconstruct the pristine `new.hbf` directly into a single, final flash
/// block, relocating the payload in the same pass. No scratch block, no
/// second copy-with-relocation pass -- this is the whole point of the
/// single-write redesign (see Tier 2 cost-accounting discussion).
///
/// Three stages, driven by one continuous decoder/reader (via `next_chunk`,
/// which transparently resumes a partially-consumed action across stage
/// boundaries):
///
/// 1. **Prefix** (`PREFIX_SIZE` bytes: `HbfHeaderBase` + `HbfHeaderMain`) --
///    buffered in RAM, since nothing can be flash-allocated yet: this is the
///    smallest amount of the reconstructed image that determines
///    `total_size()` (needed flash) and `component_min_ram()` (needed RAM).
///    Once buffered, the final block is allocated and the prefix is flushed
///    to it as its first bytes.
/// 2. **Header tail** (`PREFIX_SIZE..payload_offset`: regions, interrupts,
///    relocation table, dependencies, padding) -- written verbatim to the
///    final block, same as the full-component path's own header-copy step.
///    `payload_offset` is computable from the prefix alone (all the counts
///    it depends on live in `HbfHeaderBase`).
/// 3. **Payload** (`payload_offset..target_size`) -- each chunk is fed
///    through the same `Relocator`/`UpdateRelocator` machinery the
///    full-component path uses, writing already-relocated bytes directly to
///    the final block.
///
/// Both checksums the HBF format needs are tracked throughout, exactly as
/// the (now-removed) `install_core` did: `validation_checksum` (XOR, over
/// pre-relocation bytes, checked against the trailer embedded in the
/// reconstructed stream) and `new_checksum` (XOR, over post-relocation
/// bytes, written back as the trailer's new value).
fn reconstruct(
    channel: &mut UartChannel,
    header: &DeltaHeader,
    base_base: u32,
    base_size: u32,
) -> Result<u32, MessageError> {
    let mut reader = PatchReader::new(header.patch_payload_size as usize);
    let mut decoder = Decoder::new(header.patch_payload_size, base_size, header.target_size);
    let mut crc = Crc32State::new();
    let base_storage = Storage::new();
    let mut pending = PendingOp::None;
    let mut tmp: [u8; PACKET_BUFFER_SIZE] = [0x00; PACKET_BUFFER_SIZE];

    // --- Stage 1: buffer the prefix (HbfHeaderBase + HbfHeaderMain) in RAM ---
    // Pulls fragments via the raw channel -- no flash allocation exists yet.
    let mut prefix: [u8; PREFIX_SIZE] = [0x00; PREFIX_SIZE];
    let mut prefix_pos: usize = 0;
    {
        let mut source = FragSource::Channel(channel);
        while prefix_pos < PREFIX_SIZE {
            let n = next_chunk(
                &mut reader,
                &mut decoder,
                &mut pending,
                &mut source,
                &base_storage,
                base_base,
                &mut tmp,
                PREFIX_SIZE - prefix_pos,
                &mut crc,
            )?;
            if n == 0 {
                // Stream ended before the prefix was even fully received.
                return Err(MessageError::FailedHBFValidation);
            }
            prefix[prefix_pos..prefix_pos + n].copy_from_slice(&tmp[0..n]);
            prefix_pos += n;
        }
        // `source` (and its borrow of `channel`) is dropped at the end of
        // this block, freeing `channel` for the allocation call below.
    }

    // --- Determine allocation requirements from the prefix alone -----------
    let (needed_flash, needed_ram, payload_offset, checksum_offset) = {
        let prefix_reader = BufferReaderImpl::from(&prefix);
        let prefix_hbf = wrap_hbf_error(HbfFile::from_reader(&prefix_reader))?;
        (
            wrap_hbf_error(prefix_hbf.header_base())?.total_size(),
            wrap_hbf_error(prefix_hbf.header_main())?.component_min_ram(),
            wrap_hbf_error(prefix_hbf.get_readonly_payload())?.get_offset(),
            wrap_hbf_error(prefix_hbf.checksum_offset())?,
        )
    };
    if needed_flash != header.target_size {
        // Sanity cross-check against the delta header's own declared size.
        return Err(MessageError::FailedHBFValidation);
    }

    // --- Allocate the final block, then flush the buffered prefix into it --
    let (mut methods, final_alloc) =
        UpdateMethods::methods_for_requirements(needed_flash, needed_ram, channel).map_err(
            |e| match e {
                StorageError::OutOfFlash | StorageError::OutOfRam => MessageError::NotEnoughSpace,
                _ => MessageError::FlashError,
            },
        )?;

    let recon_result = reconstruct_into_final(
        &mut methods,
        &final_alloc,
        &mut reader,
        &mut decoder,
        &mut pending,
        &base_storage,
        base_base,
        &mut tmp,
        &prefix,
        payload_offset,
        checksum_offset,
        header,
        &mut crc,
    );
    match recon_result {
        Ok(()) => Ok(final_alloc.flash_base_address),
        Err(e) => {
            methods.deallocate();
            Err(e)
        }
    }
}

/// Continuation of `reconstruct()`: flush the buffered prefix, write the
/// header tail verbatim, then relocate the payload into `final_alloc`. Split
/// out only so `reconstruct()` can deallocate on any error via one call site.
fn reconstruct_into_final(
    methods: &mut UpdateMethods,
    final_alloc: &storage_api::AllocateComponentResponse,
    reader: &mut PatchReader,
    decoder: &mut Decoder,
    pending: &mut PendingOp,
    base_storage: &Storage,
    base_base: u32,
    tmp: &mut [u8; PACKET_BUFFER_SIZE],
    prefix: &[u8; PREFIX_SIZE],
    payload_offset: u32,
    checksum_offset: u32,
    header: &DeltaHeader,
    crc: &mut Crc32State,
) -> Result<(), MessageError> {
    let mut validation_checksum: u32 = 0;
    // `next_chunk`'s boundaries follow the delta encoding's COPY/ADD action
    // lengths, which are arbitrary (not 4-byte aligned) -- unlike the old
    // scratch-based code, whose chunking was purely `PACKET_BUFFER_SIZE`-based
    // and so always 4-byte aligned except at true region ends. Raw
    // `update_checksum` zero-pads any trailing partial word on *every* call,
    // which is only correct at a true end -- so accumulate through
    // `ChecksumBuff`, which properly carries a partial word across calls
    // (the same mechanism already used for the relocator's own checksum).
    let mut val_checksum_buff = ChecksumBuff::new();

    // --- Flush the prefix as the first PREFIX_SIZE bytes of the final block -
    val_checksum_buff.compute(&mut validation_checksum, prefix);
    methods.storage_write_stream(0, prefix, false)?;
    let mut cursor: u32 = PREFIX_SIZE as u32;

    // --- Stage 2: header tail, verbatim, up to payload_offset ---------------
    while cursor < payload_offset {
        let n = {
            let mut source = FragSource::Methods(&mut *methods);
            next_chunk(
                reader,
                decoder,
                pending,
                &mut source,
                base_storage,
                base_base,
                tmp,
                (payload_offset - cursor) as usize,
                crc,
            )?
        };
        if n == 0 {
            return Err(MessageError::FailedHBFValidation);
        }
        val_checksum_buff.compute(&mut validation_checksum, &tmp[0..n]);
        methods.storage_write_stream(cursor, &tmp[0..n], false)?;
        cursor += n as u32;
    }
    // Flush so the header is fully readable back from flash.
    methods.storage_write_stream(cursor, &[], true)?;

    // --- Parse the header now sitting on final flash; validate deps --------
    let final_reader = FlashReader::from(final_alloc.flash_base_address, final_alloc.flash_size);
    let final_hbf = wrap_hbf_error(HbfFile::from_reader(&final_reader))?;
    crate::update::validate_component_version_and_dependencies(
        &final_hbf,
        methods,
        final_alloc.flash_base_address,
    )?;
    let num_relocations = wrap_hbf_error(final_hbf.header_base())?.num_relocations();
    let payload_size = wrap_hbf_error(final_hbf.payload_size())?;

    // --- Stage 3: payload, relocated in the same pass -----------------------
    // PC7 (phase 5, revived): brackets exactly this stage -- relocate + flush
    // of the payload only -- matching bthermo-performance's RELOC marker,
    // which nests the same "relocate + flash-write" span inside its own
    // payload loop (see update.rs on that branch:
    // `markers::set(RELOC)` / `relocator.consume_current_buffer(...)` /
    // `markers::clear(RELOC)`, repeated per chunk and once more for
    // `relocator.finish`). Bracketing the whole stage here (one set/clear
    // around the loop + finish, not per-chunk) reports the same nested span
    // as one contiguous pulse rather than as bursts; RELOC_total on the
    // baseline side is itself already a *sum* of per-chunk pulses, so the
    // comparable number is this marker's total high-time either way.
    let _m_reloc = Marker::new(5);
    let mut new_checksum = validation_checksum;
    let new_flash_base_address: u32 = final_alloc.flash_base_address + 8 + payload_offset;
    let mut relocator =
        Relocator::<LINKED_FLASH_BASE, LINKED_SRAM_BASE, BUFF_SIZE, RELOC_BUFF_SIZE>::new(
            new_flash_base_address,
            final_alloc.ram_base_address,
            payload_offset as usize,
            num_relocations as usize,
        );
    let mut checksum_buff = ChecksumBuff::new();
    let end = payload_offset + payload_size;
    while cursor < end {
        let n = {
            let mut source = FragSource::Methods(&mut *methods);
            next_chunk(
                reader,
                decoder,
                pending,
                &mut source,
                base_storage,
                base_base,
                tmp,
                (end - cursor) as usize,
                crc,
            )?
        };
        if n == 0 {
            return Err(MessageError::FailedHBFValidation);
        }
        val_checksum_buff.compute(&mut validation_checksum, &tmp[0..n]);
        let mut reloc_methods = UpdateRelocator {
            hbf: &final_hbf,
            methods: &mut *methods,
            num_relocations: num_relocations as usize,
            checksum: &mut new_checksum,
        };
        relocator
            .consume_current_buffer(&tmp[0..n], &mut reloc_methods, &mut checksum_buff)
            .map_err(|_| MessageError::FlashError)?;
        cursor += n as u32;
    }
    let mut reloc_methods = UpdateRelocator {
        hbf: &final_hbf,
        methods: &mut *methods,
        num_relocations: num_relocations as usize,
        checksum: &mut new_checksum,
    };
    relocator
        .finish(&mut reloc_methods, &mut checksum_buff)
        .map_err(|_| MessageError::FlashError)?;
    drop(_m_reloc); // Stage 3 ends here -- do not let phase 5 bleed into Stage 4.

    // --- Stage 4: trailer (HbfTrailer, HBF_TRAILER_SIZE bytes) --------------
    // Unlike the full-component path (where the trailer is a separate wire
    // message, `SendComponentTrailer`), delta's continuous COPY/ADD stream
    // includes it right after the payload -- the decoder is bounded by
    // `header.target_size`, not `payload_offset + payload_size`
    // (`payload_size()` = `total_size - size_of::<HbfTrailer>() -
    // payload_offset`, confirmed in hbf_lite). Captured directly from the
    // stream rather than read back from flash: not a relocation site, and
    // (matching the old scratch-based install_core) its original value is
    // only needed transiently for the checksum comparison below -- the
    // on-flash copy gets unconditionally overwritten with `new_checksum`
    // afterward regardless, so there is no need to write the pre-relocation
    // value to flash at all. Excluded from `validation_checksum` (same as
    // the old code): it is the value validation_checksum is checked
    // *against*, not an input to it.
    static_assertions::const_assert_eq!(hbf_lite::HBF_TRAILER_SIZE, 4);
    let mut trailer_buf: [u8; 4] = [0x00; 4];
    let mut trailer_got: usize = 0;
    while trailer_got < 4 {
        let n = {
            let mut source = FragSource::Methods(&mut *methods);
            next_chunk(
                reader,
                decoder,
                pending,
                &mut source,
                base_storage,
                base_base,
                tmp,
                4 - trailer_got,
                crc,
            )?
        };
        if n == 0 {
            return Err(MessageError::FailedHBFValidation);
        }
        trailer_buf[trailer_got..trailer_got + n].copy_from_slice(&tmp[0..n]);
        trailer_got += n;
        cursor += n as u32;
    }
    if cursor != header.target_size {
        // Trailer wasn't exactly HBF_TRAILER_SIZE bytes -- format mismatch.
        return Err(MessageError::FailedHBFValidation);
    }
    let original_checksum = u32::from_le_bytes(trailer_buf);

    // --- Drain the stream (must now be exactly Done) ------------------------
    let n = {
        let mut source = FragSource::Methods(&mut *methods);
        next_chunk(
            reader,
            decoder,
            pending,
            &mut source,
            base_storage,
            base_base,
            tmp,
            PACKET_BUFFER_SIZE,
            crc,
        )?
    };
    if n != 0 {
        return Err(MessageError::FailedHBFValidation);
    }

    // --- Correctness gate: reconstructed image must match new.hbf exactly ---
    // `finalize()` takes `self` by value; swap the accumulator out since we
    // only hold `&mut Crc32State` here.
    let final_crc = core::mem::replace(crc, Crc32State::new()).finalize();
    if final_crc != header.reconstructed_crc32 {
        sys_log!("[UPDATE][delta] reconstructed CRC mismatch");
        return Err(MessageError::DeltaReconstructMismatch);
    }

    // --- HBF's own trailer checksum: compare, then write the relocated one -
    if validation_checksum != original_checksum {
        sys_log!("[UPDATE][delta] install checksum mismatch");
        return Err(MessageError::FailedHBFValidation);
    }
    methods
        .storage_write_stream(checksum_offset, &new_checksum.to_le_bytes(), true)
        .map_err(|_| MessageError::FailedHBFValidation)?;

    // --- Validate the installed final HBF ------------------------------------
    if !wrap_hbf_error(final_hbf.validate())? {
        return Err(MessageError::FailedHBFValidation);
    }
    Ok(())
}

/// Buffers the patch opcode stream, pulling `SendNextFragment` (`0xA0`)
/// fragments from the channel on demand. The device drives the transfer: each
/// pull requests `min(PACKET_BUFFER_SIZE, remaining)` bytes, CRC-8 checked.
struct PatchReader {
    remaining: usize,
    buf: [u8; PACKET_BUFFER_SIZE * 2],
    start: usize,
    end: usize,
}

impl PatchReader {
    fn new(patch_size: usize) -> Self {
        Self {
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
    fn ensure(&mut self, source: &mut FragSource, k: usize) -> Result<(), MessageError> {
        while self.available() < k && self.remaining > 0 {
            self.pull_fragment(source)?;
        }
        Ok(())
    }

    /// Read exactly `dst.len()` bytes of buffered/streamed patch data.
    fn read_into(&mut self, source: &mut FragSource, dst: &mut [u8]) -> Result<(), MessageError> {
        let mut done = 0;
        while done < dst.len() {
            if self.available() == 0 {
                if self.remaining == 0 {
                    return Err(MessageError::FailedHBFValidation);
                }
                self.pull_fragment(source)?;
            }
            let take = core::cmp::min(self.available(), dst.len() - done);
            dst[done..done + take].copy_from_slice(&self.buf[self.start..self.start + take]);
            self.start += take;
            done += take;
        }
        Ok(())
    }

    fn pull_fragment(&mut self, source: &mut FragSource) -> Result<(), MessageError> {
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
        source.pull(
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
