// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hand-crafted decoder tests.
//!
//! These tests build raw opcode byte streams manually and verify that the
//! decoder produces the expected `DecoderAction` sequence.

use delta_patcher::decoder::{Decoder, DecoderAction};
use delta_patcher::error::DecodeError;
use delta_patcher::format::{OP_ADD, OP_COPY, OP_END};

/// Helper: build a COPY opcode (9 bytes).
fn make_copy(src_offset: u32, length: u32) -> Vec<u8> {
    let mut v = vec![OP_COPY];
    v.extend_from_slice(&src_offset.to_le_bytes());
    v.extend_from_slice(&length.to_le_bytes());
    v
}

/// Helper: build an ADD opcode header + data (5 + N bytes).
fn make_add(data: &[u8]) -> Vec<u8> {
    let mut v = vec![OP_ADD];
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(data);
    v
}

/// Helper: build an END opcode (1 byte).
fn make_end() -> Vec<u8> {
    vec![OP_END]
}

// ---------------------------------------------------------------------------
//  Test: single COPY + END
// ---------------------------------------------------------------------------
#[test]
fn test_single_copy_then_end() {
    let mut stream = Vec::new();
    stream.extend(make_copy(100, 256)); // 9 bytes
    stream.extend(make_end());          // 1 byte
    let total = stream.len() as u32;    // 10

    let mut decoder = Decoder::new(total, 4096, 4096);

    // Decode COPY
    let (action, consumed) = decoder.next_action(&stream).unwrap();
    assert_eq!(consumed, 9);
    assert_eq!(action, DecoderAction::Copy { src_offset: 100, len: 256 });

    // Decode END
    let (action, consumed) = decoder.next_action(&stream[9..]).unwrap();
    assert_eq!(consumed, 1);
    assert_eq!(action, DecoderAction::Done);

    assert!(decoder.is_done());
}

// ---------------------------------------------------------------------------
//  Test: single ADD + END
// ---------------------------------------------------------------------------
#[test]
fn test_single_add_then_end() {
    let add_data = b"hello world";
    let mut stream = Vec::new();
    stream.extend(make_add(add_data)); // 5 + 11 = 16 bytes
    stream.extend(make_end());          // 1 byte
    let total = stream.len() as u32;    // 17

    let mut decoder = Decoder::new(total, 4096, 4096);

    // Decode ADD header
    let (action, consumed) = decoder.next_action(&stream).unwrap();
    assert_eq!(consumed, 5);
    assert_eq!(action, DecoderAction::Add { len: 11 });

    // Simulate caller reading 11 bytes of ADD data
    decoder.advance_add(11).unwrap();

    // Decode END
    let (action, consumed) = decoder.next_action(&stream[16..]).unwrap();
    assert_eq!(consumed, 1);
    assert_eq!(action, DecoderAction::Done);

    assert!(decoder.is_done());
}

// ---------------------------------------------------------------------------
//  Test: COPY + ADD + COPY + END  (mixed sequence)
// ---------------------------------------------------------------------------
#[test]
fn test_mixed_sequence() {
    let add_data = &[0xAA, 0xBB, 0xCC, 0xDD];
    let mut stream = Vec::new();
    stream.extend(make_copy(0, 1024));    // 9 bytes
    stream.extend(make_add(add_data));     // 5 + 4 = 9 bytes
    stream.extend(make_copy(2048, 512));   // 9 bytes
    stream.extend(make_end());             // 1 byte
    let total = stream.len() as u32;       // 28

    let mut decoder = Decoder::new(total, 4096, 4096);
    let mut pos: usize = 0;

    // COPY #1
    let (action, consumed) = decoder.next_action(&stream[pos..]).unwrap();
    assert_eq!(action, DecoderAction::Copy { src_offset: 0, len: 1024 });
    pos += consumed;

    // ADD
    let (action, consumed) = decoder.next_action(&stream[pos..]).unwrap();
    assert_eq!(action, DecoderAction::Add { len: 4 });
    pos += consumed;
    // Skip over ADD data
    decoder.advance_add(4).unwrap();
    pos += 4;

    // COPY #2
    let (action, consumed) = decoder.next_action(&stream[pos..]).unwrap();
    assert_eq!(action, DecoderAction::Copy { src_offset: 2048, len: 512 });
    pos += consumed;

    // END
    let (action, _consumed) = decoder.next_action(&stream[pos..]).unwrap();
    assert_eq!(action, DecoderAction::Done);

    assert!(decoder.is_done());
}

// ---------------------------------------------------------------------------
//  Test: BufferTooShort error
// ---------------------------------------------------------------------------
#[test]
fn test_buffer_too_short_copy() {
    let stream = make_copy(0, 100);
    let mut decoder = Decoder::new(10, 4096, 4096);
    // Only feed 5 of the 9 needed bytes
    let result = decoder.next_action(&stream[..5]);
    assert_eq!(result, Err(DecodeError::BufferTooShort));
}

#[test]
fn test_buffer_too_short_add() {
    let stream = make_add(&[0x01, 0x02]);
    let mut decoder = Decoder::new(10, 4096, 4096);
    // Only feed 3 of the 5 needed header bytes
    let result = decoder.next_action(&stream[..3]);
    assert_eq!(result, Err(DecodeError::BufferTooShort));
}

#[test]
fn test_empty_buffer() {
    let mut decoder = Decoder::new(10, 4096, 4096);
    let result = decoder.next_action(&[]);
    assert_eq!(result, Err(DecodeError::BufferTooShort));
}

// ---------------------------------------------------------------------------
//  Test: unknown opcode
// ---------------------------------------------------------------------------
#[test]
fn test_unknown_opcode() {
    let mut decoder = Decoder::new(10, 4096, 4096);
    let result = decoder.next_action(&[0xFF]);
    assert_eq!(result, Err(DecodeError::UnknownOpcode(0xFF)));
}

// ---------------------------------------------------------------------------
//  Test: premature END (consumed != total_size)
// ---------------------------------------------------------------------------
#[test]
fn test_premature_end() {
    // Declare 20 bytes of payload, but END after only 1 byte consumed
    let mut decoder = Decoder::new(20, 4096, 4096);
    let result = decoder.next_action(&[OP_END]);
    assert_eq!(result, Err(DecodeError::PrematureEnd));
}

// ---------------------------------------------------------------------------
//  Test: mixed sequence with report file output
// ---------------------------------------------------------------------------
#[test]
fn test_mixed_sequence_with_report() {
    use delta_patcher::decoder::decode_to_report;
    use std::path::Path;

    // Build a 4096-byte base image
    let mut old = vec![0u8; 4096];
    for i in 0..old.len() {
        old[i] = (i % 251) as u8;
    }

    let add_data = &[0xAA, 0xBB, 0xCC, 0xDD];
    let mut stream = Vec::new();
    stream.extend(make_copy(0, 1024));    // 9 bytes
    stream.extend(make_add(add_data));     // 5 + 4 = 9 bytes
    stream.extend(make_copy(2048, 512));   // 9 bytes
    stream.extend(make_end());             // 1 byte

    let report_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("output");
    std::fs::create_dir_all(&report_dir).expect("cannot create output dir");
    let report_path = report_dir.join("decoder_mixed_report.txt");

    let target_size: u32 = 1024 + 4 + 512; // 1540
    let reconstructed = decode_to_report(&stream, &old, target_size, &report_path).unwrap();

    // Verify reconstructed data
    assert_eq!(reconstructed.len(), 1024 + 4 + 512); // 1540 bytes
    assert_eq!(&reconstructed[0..1024], &old[0..1024]);
    assert_eq!(&reconstructed[1024..1028], add_data);
    assert_eq!(&reconstructed[1028..1540], &old[2048..2560]);

    // Verify report file
    assert!(report_path.exists(), "report file should exist");
    let report = std::fs::read_to_string(&report_path).unwrap();
    assert!(report.contains("=== Delta Decoder Report ==="));
    assert!(report.contains("COPY"));
    assert!(report.contains("ADD"));
    assert!(report.contains("END"));
    assert!(report.contains("1540 bytes reconstructed"));
    assert!(report.contains("reconstructed CRC32"));
}
// ---------------------------------------------------------------------------
//  Test: CopyOutOfBounds — COPY reads past base image
// ---------------------------------------------------------------------------
#[test]
fn test_copy_out_of_bounds() {
    // base_size = 1000, COPY tries to read base[900..1100]
    let mut stream = Vec::new();
    stream.extend(make_copy(900, 200)); // src_offset=900, len=200 => 900+200=1100 > 1000
    stream.extend(make_end());
    let total = stream.len() as u32;

    let mut decoder = Decoder::new(total, 1000, 4096);
    let result = decoder.next_action(&stream);
    assert_eq!(result, Err(DecodeError::CopyOutOfBounds));
}

// ---------------------------------------------------------------------------
//  Test: OutputOverflow — COPY exceeds target size
// ---------------------------------------------------------------------------
#[test]
fn test_output_overflow_copy() {
    // target_size = 100, COPY len = 200
    let mut stream = Vec::new();
    stream.extend(make_copy(0, 200));
    stream.extend(make_end());
    let total = stream.len() as u32;

    let mut decoder = Decoder::new(total, 4096, 100);
    let result = decoder.next_action(&stream);
    assert_eq!(result, Err(DecodeError::OutputOverflow));
}

// ---------------------------------------------------------------------------
//  Test: OutputOverflow — ADD exceeds target size
// ---------------------------------------------------------------------------
#[test]
fn test_output_overflow_add() {
    // target_size = 5, ADD len = 10
    let add_data = &[0xAA; 10];
    let mut stream = Vec::new();
    stream.extend(make_add(add_data));
    stream.extend(make_end());
    let total = stream.len() as u32;

    let mut decoder = Decoder::new(total, 4096, 5);
    let result = decoder.next_action(&stream);
    assert_eq!(result, Err(DecodeError::OutputOverflow));
}

// ---------------------------------------------------------------------------
//  Test: OutputOverflow — cumulative overflow across multiple opcodes
// ---------------------------------------------------------------------------
#[test]
fn test_output_overflow_cumulative() {
    // target_size = 1500, COPY 1024 + ADD 4 + COPY 512 = 1540 > 1500
    let add_data = &[0xAA, 0xBB, 0xCC, 0xDD];
    let mut stream = Vec::new();
    stream.extend(make_copy(0, 1024));
    stream.extend(make_add(add_data));
    stream.extend(make_copy(2048, 512));
    stream.extend(make_end());
    let total = stream.len() as u32;

    let mut decoder = Decoder::new(total, 4096, 1500);
    let mut pos: usize = 0;

    // COPY 1024 — ok (write_pos = 1024 <= 1500)
    let (action, consumed) = decoder.next_action(&stream[pos..]).unwrap();
    assert_eq!(action, DecoderAction::Copy { src_offset: 0, len: 1024 });
    pos += consumed;

    // ADD 4 — ok (write_pos = 1028 <= 1500)
    let (action, consumed) = decoder.next_action(&stream[pos..]).unwrap();
    assert_eq!(action, DecoderAction::Add { len: 4 });
    pos += consumed;
    decoder.advance_add(4).unwrap();
    pos += 4;

    // COPY 512 — fails! (write_pos = 1028 + 512 = 1540 > 1500)
    let result = decoder.next_action(&stream[pos..]);
    assert_eq!(result, Err(DecodeError::OutputOverflow));
}

// ---------------------------------------------------------------------------
//  Finding 2: a stream that runs out of declared payload without an END
//  opcode must surface `MissingEnd` (previously it looped forever on
//  `BufferTooShort`).
// ---------------------------------------------------------------------------
#[test]
fn test_missing_end_truncated_stream() {
    // A single COPY (9 bytes) declared as the entire payload, but no END.
    let stream = make_copy(0, 256); // 9 bytes
    let total = stream.len() as u32; // 9

    let mut decoder = Decoder::new(total, 4096, 4096);

    // First call decodes the COPY and consumes all 9 declared bytes.
    let (action, consumed) = decoder.next_action(&stream).unwrap();
    assert_eq!(action, DecoderAction::Copy { src_offset: 0, len: 256 });
    assert_eq!(consumed, 9);

    // Second call: all payload consumed, no END seen -> MissingEnd (not a
    // perpetual BufferTooShort).
    let result = decoder.next_action(&stream[9..]);
    assert_eq!(result, Err(DecodeError::MissingEnd));
}

// ---------------------------------------------------------------------------
//  Finding 2 (cont.): an opcode whose header would push `consumed` past the
//  declared payload size is rejected as `OverlongStream`.
// ---------------------------------------------------------------------------
#[test]
fn test_overlong_stream_copy_past_declared_size() {
    // Declare a payload of only 5 bytes but feed a 9-byte COPY.
    let stream = make_copy(0, 16);
    let mut decoder = Decoder::new(5, 4096, 4096);
    let result = decoder.next_action(&stream);
    assert_eq!(result, Err(DecodeError::OverlongStream));
}

#[test]
fn test_overlong_stream_add_data_past_declared_size() {
    // ADD header(5) + data(8) = 13 bytes, but declared payload is only 10.
    let stream = make_add(&[0xAB; 8]);
    let mut decoder = Decoder::new(10, 4096, 4096);
    let result = decoder.next_action(&stream);
    assert_eq!(result, Err(DecodeError::OverlongStream));
}

// ---------------------------------------------------------------------------
//  Finding 3: `advance_add` validates its argument against the pending ADD.
//  A corrupted length must be rejected rather than silently desynchronising
//  the consumed counter (which previously wrapped in release builds).
// ---------------------------------------------------------------------------
#[test]
fn test_advance_add_length_mismatch_rejected() {
    let mut stream = Vec::new();
    stream.extend(make_add(&[1, 2, 3, 4])); // len 4
    stream.extend(make_end());
    let total = stream.len() as u32;

    let mut decoder = Decoder::new(total, 4096, 4096);
    let (action, _consumed) = decoder.next_action(&stream).unwrap();
    assert_eq!(action, DecoderAction::Add { len: 4 });

    // Caller lies about how many bytes it consumed.
    assert_eq!(decoder.advance_add(7), Err(DecodeError::AddLengthMismatch));
}

#[test]
fn test_advance_add_without_pending_rejected() {
    // No ADD pending (fresh decoder) -> mismatch.
    let mut decoder = Decoder::new(10, 4096, 4096);
    assert_eq!(decoder.advance_add(4), Err(DecodeError::AddLengthMismatch));
}

#[test]
fn test_next_action_blocked_while_add_pending() {
    let mut stream = Vec::new();
    stream.extend(make_add(&[1, 2, 3, 4]));
    stream.extend(make_end());
    let total = stream.len() as u32;

    let mut decoder = Decoder::new(total, 4096, 4096);
    let (action, consumed) = decoder.next_action(&stream).unwrap();
    assert_eq!(action, DecoderAction::Add { len: 4 });

    // Calling next_action before advance_add is a contract violation.
    let result = decoder.next_action(&stream[consumed..]);
    assert_eq!(result, Err(DecodeError::PendingAddData));
}
