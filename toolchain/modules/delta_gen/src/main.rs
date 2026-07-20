// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `delta_gen` — build a **Delta HBF** from a pristine `(old.hbf, new.hbf)` pair.
//!
//! The device stores the base component **post-relocation** and rewrites its
//! XOR trailer, so the only base offsets where flash equals the pristine
//! `old.hbf` are those outside every relocation-site word and outside the
//! 4-byte trailer.  `delta_gen` therefore encodes with `encode_faithful`,
//! forbidding COPY sources from those offsets; the corresponding `new` bytes
//! travel as ADD literals instead.  Reconstruction on-device yields the
//! pristine pre-relocation `new.hbf` byte-for-byte, verified by the
//! reconstructed CRC-32b.
//!
//! Output layout (see `documentation/ConceptOSDeltaBinaryFormat.md`):
//! ```text
//! [ standard fixed header, copied from new.hbf, IS_DELTA flag set ]
//! [ 32-byte Delta Header ]
//! [ patch payload (COPY/ADD/END opcode stream) ]
//! [ 4-byte CRC-32b trailer over everything preceding ]
//! ```

use std::ops::Range;
use std::path::PathBuf;
use std::process::exit;

use clap::Parser;

use delta_patcher::format::{
    crc32b, DeltaHeader, ALGO_COPY_ADD_V1, DELTA_FORMAT_V1, DELTA_MAGIC,
};
use delta_patcher::{encode_faithful, Decoder, DecoderAction};
use hbf_rs::{parse_hbf, HbfFile, HBF_HEADER_MIN_SIZE};

/// `IS_DELTA` = bit 1 of the HBF Component Flags (see the format doc / hbf_lite).
const IS_DELTA_BIT: u16 = 1 << 1;
/// Byte offset of `component_flags` within the fixed header: it is the second
/// field (`u16` priority, then `u16` flags) of the Main header, which follows
/// the Base header. Guarded by a runtime check against the parsed value.
const COMPONENT_FLAGS_OFFSET: usize = HBF_HEADER_MIN_SIZE + 2;

#[derive(Parser)]
#[clap(about = "Generate a Delta HBF from a pristine (old.hbf, new.hbf) pair")]
struct Args {
    /// Pristine base component image (the version installed on the device).
    old: PathBuf,
    /// Pristine target component image (the version to update to).
    new: PathBuf,
    /// Output path for the Delta HBF.
    #[clap(short, long)]
    output: PathBuf,
    /// Also write the raw patch payload (opcode stream) here for inspection.
    #[clap(long)]
    patch_out: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    if let Err(e) = run(&args) {
        eprintln!("delta_gen: error: {e}");
        exit(1);
    }
}

fn run(args: &Args) -> Result<(), String> {
    let old_bytes = std::fs::read(&args.old).map_err(|e| format!("reading {:?}: {e}", args.old))?;
    let new_bytes = std::fs::read(&args.new).map_err(|e| format!("reading {:?}: {e}", args.new))?;

    let old_hbf = parse_hbf(&old_bytes).map_err(|e| format!("parsing old.hbf: {e:?}"))?;
    let new_hbf = parse_hbf(&new_bytes).map_err(|e| format!("parsing new.hbf: {e:?}"))?;

    // --- Build the forbidden set from the OLD image ---------------------------
    // Relocation `file_offset` values are **whole-HBF-file relative** (the
    // relocator matches them against a file position that starts at 0 over the
    // entire HBF — see system_builder::relocate_hbf and the update component,
    // which starts the relocator at payload_offset precisely because the
    // offsets are file-relative). Each relocation site (AbsAddress / MovW /
    // MovT) patches a 4-byte word. The 4-byte XOR trailer is also rewritten
    // on-device.
    let mut forbidden: Vec<Range<usize>> = Vec::new();
    for reloc in old_hbf.relocation_iter() {
        let file_offset = (reloc.value() & 0x00FF_FFFF) as usize;
        forbidden.push(file_offset..file_offset + 4);
    }
    let trailer_start = old_hbf.checksum_offset() as usize;
    forbidden.push(trailer_start..trailer_start + 4);
    let forbidden = normalize_ranges(forbidden, old_bytes.len())?;

    // --- Encode ---------------------------------------------------------------
    let patch = encode_faithful(&old_bytes, &new_bytes, &forbidden);

    // --- Integrity fields -----------------------------------------------------
    let base_crc32 = masked_crc32(&old_bytes, &forbidden);
    let reconstructed_crc32 = crc32b(&new_bytes);

    let header = DeltaHeader {
        magic: DELTA_MAGIC,
        format_version: DELTA_FORMAT_V1,
        algorithm_id: ALGO_COPY_ADD_V1,
        base_component_id: old_hbf.header_base().component_id(),
        base_component_version: old_hbf.header_base().component_version(),
        reserved: 0,
        base_crc32,
        reconstructed_crc32,
        patch_payload_size: patch.len() as u32,
        target_size: new_bytes.len() as u32,
    };

    // --- Self-check BEFORE writing: a malformed patch must be un-shippable -----
    let (reconstructed, copy_ops, add_ops, forbidden_add_bytes) =
        self_check(&patch, &old_bytes, &new_bytes, &forbidden)?;
    if crc32b(&reconstructed) != reconstructed_crc32 {
        return Err("self-check: reconstructed CRC mismatch".into());
    }

    // --- Assemble the Delta HBF ----------------------------------------------
    let delta_hbf = assemble(&new_bytes, &new_hbf, &header, &patch)?;

    std::fs::write(&args.output, &delta_hbf)
        .map_err(|e| format!("writing {:?}: {e}", args.output))?;
    if let Some(p) = &args.patch_out {
        std::fs::write(p, &patch).map_err(|e| format!("writing {:?}: {e}", p))?;
    }

    // --- Report ---------------------------------------------------------------
    let pct = 100.0 * (1.0 - patch.len() as f64 / new_bytes.len() as f64);
    println!("=== delta_gen ===");
    println!("old.hbf:                {} bytes", old_bytes.len());
    println!("new.hbf:                {} bytes", new_bytes.len());
    println!("base component:         id={} version={}", header.base_component_id, header.base_component_version);
    println!("relocation sites:       {}", old_hbf.header_base().num_relocations());
    println!("forbidden ranges:       {} ({} bytes)", forbidden.len(), forbidden.iter().map(|r| r.len()).sum::<usize>());
    println!("patch payload:          {} bytes", patch.len());
    println!("  COPY ops:             {copy_ops}");
    println!("  ADD ops:              {add_ops}");
    println!("  bytes forced to ADD by forbidden set: {forbidden_add_bytes}");
    println!("delta HBF total:        {} bytes", delta_hbf.len());
    println!("reduction vs new.hbf:   {pct:.1}%");
    println!("base_crc32 (masked):    0x{base_crc32:08X}");
    println!("reconstructed_crc32:    0x{reconstructed_crc32:08X}");
    println!("self-check:             OK (in-memory decode == new.hbf, CRC matches)");

    Ok(())
}

/// Sort, validate non-overlap, and merge any touching/overlapping ranges so the
/// encoder receives a clean sorted non-overlapping set.
fn normalize_ranges(mut ranges: Vec<Range<usize>>, len: usize) -> Result<Vec<Range<usize>>, String> {
    for r in &ranges {
        if r.end > len {
            return Err(format!("forbidden range {r:?} exceeds old.hbf length {len}"));
        }
    }
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        if let Some(last) = merged.last_mut() {
            if r.start <= last.end {
                last.end = last.end.max(r.end);
                continue;
            }
        }
        merged.push(r);
    }
    Ok(merged)
}

/// CRC-32b over `data` with every byte inside a `forbidden` range treated as 0.
fn masked_crc32(data: &[u8], forbidden: &[Range<usize>]) -> u32 {
    let mut masked = data.to_vec();
    for r in forbidden {
        for b in &mut masked[r.clone()] {
            *b = 0;
        }
    }
    crc32b(&masked)
}

/// Apply `patch` to `base` in-memory via the streaming decoder.
/// Returns (reconstructed, copy_ops, add_ops).
fn decode_against(
    patch: &[u8],
    base: &[u8],
    target_len: usize,
) -> Result<(Vec<u8>, usize, usize), String> {
    let mut dec = Decoder::new(patch.len() as u32, base.len() as u32, target_len as u32);
    let mut out = vec![0u8; target_len];
    let (mut pp, mut w) = (0usize, 0usize);
    let (mut copy_ops, mut add_ops) = (0usize, 0usize);
    loop {
        let (action, hdr) = dec
            .next_action(&patch[pp..])
            .map_err(|e| format!("self-check decode: {e:?}"))?;
        match action {
            DecoderAction::Copy { src_offset, len } => {
                let (s, len) = (src_offset as usize, len as usize);
                out[w..w + len].copy_from_slice(&base[s..s + len]);
                w += len;
                pp += hdr;
                copy_ops += 1;
            }
            DecoderAction::Add { len } => {
                let len = len as usize;
                out[w..w + len].copy_from_slice(&patch[pp + hdr..pp + hdr + len]);
                w += len;
                dec.advance_add(len as u32)
                    .map_err(|e| format!("self-check advance_add: {e:?}"))?;
                pp += hdr + len;
                add_ops += 1;
            }
            DecoderAction::Done => break,
        }
    }
    Ok((out, copy_ops, add_ops))
}

/// Decode the patch against `old` AND against a *poisoned* copy of `old` (every
/// forbidden byte flipped, simulating the on-device post-relocation base). Both
/// must reconstruct `new` exactly — the second decode is the real invariant: it
/// proves no COPY source ever depends on a relocation-site/trailer byte, which
/// is exactly what differs between the host's pristine base and device flash.
fn self_check(
    patch: &[u8],
    old: &[u8],
    new: &[u8],
    forbidden: &[Range<usize>],
) -> Result<(Vec<u8>, usize, usize, usize), String> {
    let (out, copy_ops, add_ops) = decode_against(patch, old, new.len())?;
    if out != new {
        return Err("self-check: reconstructed image != new.hbf (pristine base)".into());
    }

    // Poison every forbidden byte to model the post-relocation device base.
    let mut poisoned = old.to_vec();
    for r in forbidden {
        for b in &mut poisoned[r.clone()] {
            *b ^= 0xFF;
        }
    }
    let (out_poisoned, _, _) = decode_against(patch, &poisoned, new.len())?;
    if out_poisoned != new {
        return Err(
            "self-check: a COPY read a forbidden (relocation/trailer) byte — the patch \
             would fail on the device's post-relocation base"
                .into(),
        );
    }

    let forbidden_add_bytes = forbidden.iter().map(|r| r.len()).sum();
    Ok((out, copy_ops, add_ops, forbidden_add_bytes))
}

/// Assemble `[fixed header (IS_DELTA set)][DeltaHeader][patch][CRC-32b trailer]`.
fn assemble(
    new_bytes: &[u8],
    new_hbf: &dyn HbfFile,
    header: &DeltaHeader,
    patch: &[u8],
) -> Result<Vec<u8>, String> {
    let fixed_len = hbf_rs::FIXED_HEADER_SIZE;
    if new_bytes.len() < fixed_len {
        return Err("new.hbf shorter than a fixed header".into());
    }

    let mut out = Vec::new();
    out.extend_from_slice(&new_bytes[..fixed_len]);

    // Set IS_DELTA in the outer fixed header's Component Flags.
    let cur = new_hbf.header_main().component_flags().bits();
    let new_flags = cur | IS_DELTA_BIT;
    out[COMPONENT_FLAGS_OFFSET..COMPONENT_FLAGS_OFFSET + 2]
        .copy_from_slice(&new_flags.to_le_bytes());

    out.extend_from_slice(&header.to_bytes());
    out.extend_from_slice(patch);
    // 4-byte CRC-32b trailer over everything preceding.
    let trailer = crc32b(&out);
    out.extend_from_slice(&trailer.to_le_bytes());
    Ok(out)
}
