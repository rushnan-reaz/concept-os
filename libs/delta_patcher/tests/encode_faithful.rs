// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tests for the relocation-aware encoder (`encode_faithful`).
//!
//! Two invariants are checked:
//!   1. No emitted COPY source range ever overlaps a `forbidden` range.
//!   2. The patch still reconstructs `new` byte-for-byte via the decoder.

use std::ops::Range;

use delta_patcher::decoder::{Decoder, DecoderAction};
use delta_patcher::encode_faithful;
use delta_patcher::format::{OP_ADD, OP_COPY, OP_END};

/// Walk a patch stream and return every COPY's `[src_offset, src_offset+len)`.
fn copy_ranges(patch: &[u8]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut p = 0usize;
    while p < patch.len() {
        match patch[p] {
            OP_COPY => {
                let off = u32::from_le_bytes(patch[p + 1..p + 5].try_into().unwrap()) as usize;
                let len = u32::from_le_bytes(patch[p + 5..p + 9].try_into().unwrap()) as usize;
                ranges.push(off..off + len);
                p += 9;
            }
            OP_ADD => {
                let len = u32::from_le_bytes(patch[p + 1..p + 5].try_into().unwrap()) as usize;
                p += 5 + len;
            }
            OP_END => break,
            other => panic!("bad opcode 0x{other:02x} at {p}"),
        }
    }
    ranges
}

fn ranges_overlap(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Apply a patch to `old` via the streaming decoder and return the output.
fn decode(patch: &[u8], old: &[u8], target_size: usize) -> Vec<u8> {
    let mut dec = Decoder::new(patch.len() as u32, old.len() as u32, target_size as u32);
    let mut out = vec![0u8; target_size];
    let mut pp = 0usize;
    let mut w = 0usize;
    loop {
        let (action, hdr) = dec.next_action(&patch[pp..]).expect("decode");
        match action {
            DecoderAction::Copy { src_offset, len } => {
                let len = len as usize;
                let s = src_offset as usize;
                out[w..w + len].copy_from_slice(&old[s..s + len]);
                w += len;
                pp += hdr;
            }
            DecoderAction::Add { len } => {
                let len = len as usize;
                out[w..w + len].copy_from_slice(&patch[pp + hdr..pp + hdr + len]);
                w += len;
                dec.advance_add(len as u32).unwrap();
                pp += hdr + len;
            }
            DecoderAction::Done => break,
        }
    }
    out
}

/// Assert both invariants for a given (old, new, forbidden).
fn check(old: &[u8], new: &[u8], forbidden: &[Range<usize>]) {
    let patch = encode_faithful(old, new, forbidden);

    // Invariant 1: no COPY source touches a forbidden range.
    for c in copy_ranges(&patch) {
        for f in forbidden {
            assert!(
                !ranges_overlap(&c, f),
                "COPY {c:?} overlaps forbidden {f:?}"
            );
        }
    }

    // Invariant 2: exact reconstruction.
    let out = decode(&patch, old, new.len());
    assert_eq!(out, new, "roundtrip mismatch");
}

#[test]
fn faithful_empty_forbidden_matches_plain() {
    // With no forbidden ranges, encode_faithful == encode.
    let old: Vec<u8> = (0..2000).map(|i| (i % 251) as u8).collect();
    let mut new = old.clone();
    new[500] ^= 0xFF;
    assert_eq!(
        encode_faithful(&old, &new, &[]),
        delta_patcher::encode(&old, &new)
    );
}

#[test]
fn faithful_forbidden_forces_add_not_copy() {
    // old == new, but a mid-stream 4-byte range is forbidden. The encoder must
    // not COPY across it — those bytes come as ADD — yet reconstruction is exact.
    let old: Vec<u8> = (0..1000).map(|i| (i * 7 % 256) as u8).collect();
    let new = old.clone();
    // Simulate a relocation-site word at offset 400 plus a trailer at the end.
    let forbidden = [400..404, (old.len() - 4)..old.len()];
    check(&old, &new, &forbidden);
}

#[test]
fn faithful_multiple_scattered_forbidden() {
    let old: Vec<u8> = (0..4096).map(|i| (i % 253) as u8).collect();
    let mut new = old.clone();
    // A couple of real edits away from the forbidden sites.
    for b in new.iter_mut().skip(1500).take(20) {
        *b = b.wrapping_add(1);
    }
    let forbidden = [100..104, 108..112, 2000..2004, 4092..4096];
    check(&old, &new, &forbidden);
}

#[test]
fn faithful_forbidden_at_offset_zero() {
    let old: Vec<u8> = (0..512).map(|i| (i % 97) as u8).collect();
    let new = old.clone();
    let forbidden = [0..4];
    check(&old, &new, &forbidden);
}

/// Deterministic pseudo-random fuzz: random data, random edits, random
/// non-overlapping forbidden sets. Both invariants must always hold.
#[test]
fn faithful_fuzz_random_forbidden() {
    // Small xorshift PRNG so the test is deterministic and dependency-free.
    let mut state: u64 = 0x1234_5678_9abc_def0;
    let mut rng = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    for _ in 0..200 {
        let len = 200 + (rng() as usize % 2000);
        let old: Vec<u8> = (0..len).map(|_| (rng() & 0xFF) as u8).collect();

        // new = old with a handful of random single-byte edits.
        let mut new = old.clone();
        let edits = rng() as usize % 8;
        for _ in 0..edits {
            let idx = rng() as usize % len;
            new[idx] = (rng() & 0xFF) as u8;
        }

        // Build a sorted, non-overlapping forbidden set of 4-byte words.
        let mut forbidden: Vec<Range<usize>> = Vec::new();
        let mut cursor = 0usize;
        while cursor + 4 <= len {
            let gap = rng() as usize % 64;
            cursor += gap;
            if cursor + 4 > len {
                break;
            }
            forbidden.push(cursor..cursor + 4);
            cursor += 4;
            if forbidden.len() >= 16 {
                break;
            }
        }

        check(&old, &new, &forbidden);
    }
}
