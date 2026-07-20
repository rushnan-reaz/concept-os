// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Round-trip tests: encode(old, new) → decode + apply → assert output == new.
//!
//! These tests exercise both the encoder and decoder together.

use delta_patcher::decoder::{Decoder, DecoderAction};
use delta_patcher::encoder::encode;
use delta_patcher::format::OP_END;

/// Apply a patch opcode stream against `old` to reconstruct `new`.
/// Simulates what the device-side applicator does.
fn apply_patch(old: &[u8], patch: &[u8], target_size: usize) -> Vec<u8> {
    let mut decoder = Decoder::new(patch.len() as u32, old.len() as u32, target_size as u32);
    let mut output = Vec::new();
    let mut patch_pos: usize = 0;

    loop {
        let (action, consumed) = decoder
            .next_action(&patch[patch_pos..])
            .expect("decoder error");
        patch_pos += consumed;

        match action {
            DecoderAction::Copy { src_offset, len } => {
                let start = src_offset as usize;
                let end = start + len as usize;
                output.extend_from_slice(&old[start..end]);
            }
            DecoderAction::Add { len } => {
                let len = len as usize;
                let data = &patch[patch_pos..patch_pos + len];
                output.extend_from_slice(data);
                decoder.advance_add(len as u32).unwrap();
                patch_pos += len;
            }
            DecoderAction::Done => break,
        }
    }

    output
}

// ---------------------------------------------------------------------------
//  Test: identical inputs → trivially one big COPY
// ---------------------------------------------------------------------------
#[test]
fn roundtrip_identical() {
    let data = b"The quick brown fox jumps over the lazy dog. Repeating content here.";
    let patch = encode(data, data);
    let reconstructed = apply_patch(data, &patch, data.len());
    assert_eq!(reconstructed, data);
}

// ---------------------------------------------------------------------------
//  Test: completely different inputs → all ADD
// ---------------------------------------------------------------------------
#[test]
fn roundtrip_completely_different() {
    let old = vec![0xAA; 100];
    let new = vec![0xBB; 100];
    let patch = encode(&old, &new);
    let reconstructed = apply_patch(&old, &patch, new.len());
    assert_eq!(reconstructed, new);
}

// ---------------------------------------------------------------------------
//  Test: empty new → output is just END
// ---------------------------------------------------------------------------
#[test]
fn roundtrip_empty_new() {
    let old = b"some old data";
    let new: &[u8] = &[];
    let patch = encode(old, new);
    assert_eq!(patch, vec![OP_END]);
    let reconstructed = apply_patch(old, &patch, new.len());
    assert_eq!(reconstructed, new);
}

// ---------------------------------------------------------------------------
//  Test: append-only change
// ---------------------------------------------------------------------------
#[test]
fn roundtrip_append() {
    let old = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // 40 bytes
    let mut new = old.to_vec();
    new.extend_from_slice(b"BBBBBBBBBBBBBBBBBBBB"); // append 20 bytes
    let patch = encode(old, &new);
    let reconstructed = apply_patch(old, &patch, new.len());
    assert_eq!(reconstructed, new);
}

// ---------------------------------------------------------------------------
//  Test: realistic small change in the middle
// ---------------------------------------------------------------------------
#[test]
fn roundtrip_middle_edit() {
    // Old: a 200-byte block of structured data
    let mut old = Vec::new();
    for i in 0u8..200 {
        old.push(i);
    }
    // New: same, but bytes 80..90 are changed
    let mut new = old.clone();
    for i in 80..90 {
        new[i] = 0xFF;
    }
    let patch = encode(&old, &new);
    let reconstructed = apply_patch(&old, &patch, new.len());
    assert_eq!(reconstructed, new);

    // Verify the patch is smaller than the full new image
    assert!(
        patch.len() < new.len(),
        "patch ({} B) should be smaller than new ({} B)",
        patch.len(),
        new.len()
    );
}

// ---------------------------------------------------------------------------
//  Test: larger image with scattered changes
// ---------------------------------------------------------------------------
// #[test]
// fn roundtrip_scattered_changes() {
//     // Build a 4096-byte "CBF-like" image
//     let mut old = Vec::new();
//     for i in 0..4096u32 {
//         old.push((i % 251) as u8); // pseudo-random-ish pattern
//     }
//     // Make scattered changes
//     let mut new = old.clone();
//     // Change bytes 100..120
//     for i in 100..120 {
//         new[i] = 0xDE;
//     }
//     // Change bytes 2000..2050
//     for i in 2000..2050 {
//         new[i] = 0xAD;
//     }
//     // Change bytes 3800..3850
//     for i in 3800..3850 {
//         new[i] = 0xBE;
//     }

//     let patch = encode(&old, &new);
//     let reconstructed = apply_patch(&old, &patch, new.len());
//     assert_eq!(reconstructed, new);

//     // Verify meaningful size reduction
//     let reduction = 1.0 - (patch.len() as f64 / new.len() as f64);
//     assert!(
//         reduction > 0.5,
//         "expected >50% reduction, got {:.1}% (patch {} B vs new {} B)",
//         reduction * 100.0,
//         patch.len(),
//         new.len()
//     );
// }

// ---------------------------------------------------------------------------
//  Test: scattered changes with report file output
// ---------------------------------------------------------------------------
#[test]
fn roundtrip_scattered_changes_with_report() {
    use delta_patcher::encoder::encode_to_report;
    use std::path::Path;

    // "CBF-like" image
    let mut old = Vec::new();
    for i in 0..4096u32 {
        old.push((i % 251) as u8);
    }

    // Scattered changes
    let mut new = old.clone();
    for i in 100..120 {
        new[i] = 0xDE;
    }
    for i in 2000..2050 {
        new[i] = 0xAD;
    }
    for i in 3800..3850 {
        new[i] = 0xBE;
    }

    // Write report to file
    let report_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("output");
    std::fs::create_dir_all(&report_dir).expect("cannot create output dir");
    let report_path = report_dir.join("scattered_changes_report.txt");

    let patch = encode_to_report(&old, &new, &report_path);
    let reconstructed = apply_patch(&old, &patch, new.len());
    assert_eq!(reconstructed, new);

    // Verify report file was written
    assert!(report_path.exists(), "report file should exist");
    let report = std::fs::read_to_string(&report_path).unwrap();
    assert!(report.contains("=== Delta Encoder ==="));
    assert!(report.contains("Hash Table"));
    assert!(report.contains("COPY"));
    assert!(report.contains("ADD"));
    assert!(report.contains("END"));
}
