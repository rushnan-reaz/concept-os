// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Delta opcode encoder (host-side, `std` only).
//!
//! Takes two byte slices (`old` and `new` CBF images) and produces a COPY/ADD
//! opcode stream that, when applied to `old`, reconstructs `new`.

// The whole module is gated behind `feature = "std"` in `lib.rs`, so no
// per-item `cfg` is needed here.
mod inner {
    extern crate std;
    use std::collections::HashMap;
    use std::io::Write;
    use std::ops::Range;
    use std::path::Path;
    use std::vec::Vec;

    use crate::format::{OP_ADD, OP_COPY, OP_END};

    /// Minimum match length (bytes).  Matches shorter than this are emitted as
    /// ADD because the COPY overhead (9 bytes) would exceed the saving.
    const MIN_MATCH_LEN: usize = 10;

    /// Maximum number of hash-table candidates to evaluate per position.
    /// Caps worst-case search time on highly repetitive data.
    const MAX_CANDIDATES: usize = 64;

    /// Rolling hash window size.
    const WINDOW_SIZE: usize = 16;

    /// Encode a delta opcode stream from `old` to `new`.
    ///
    /// Returns a `Vec<u8>` containing the raw opcode bytes (COPY/ADD/END).
    /// This is the patch payload that goes into the Delta CBF between the
    /// Delta Header and the Trailer.
    pub fn encode(old: &[u8], new: &[u8]) -> Vec<u8> {
        encode_core(old, new, &[], None)
    }

    /// Relocation-aware encode: like [`encode`], but guarantees no emitted COPY
    /// source range ever overlaps a byte in `forbidden`.
    ///
    /// `forbidden` is a **sorted, non-overlapping** set of `old`-offset byte
    /// ranges that COPY must never cover — on the device these are the
    /// relocation-site words (whose flash bytes are post-relocation and differ
    /// from pristine `old`) and the 4-byte XOR trailer.  Any `new` byte that
    /// would otherwise have been reconstructed by copying a forbidden `old`
    /// byte is instead emitted as an ADD literal carrying the pristine `new`
    /// byte, so reconstruction yields the pristine pre-relocation `new` image.
    pub fn encode_faithful(old: &[u8], new: &[u8], forbidden: &[Range<usize>]) -> Vec<u8> {
        encode_core(old, new, forbidden, None)
    }

    /// Same as [`encode`], but writes the hash table and each emitted opcode
    /// to a report file at `path`.
    pub fn encode_to_report(old: &[u8], new: &[u8], path: &Path) -> Vec<u8> {
        let file = std::fs::File::create(path)
            .expect(&std::format!("cannot create report file: {}", path.display()));
        let mut writer = std::io::BufWriter::new(file);
        let result = encode_core(old, new, &[], Some(&mut writer));
        writer.flush().expect("failed to flush report file");
        result
    }

    /// Largest match length starting at `old` offset `offset` that does not
    /// enter any `forbidden` range.  Returns 0 if `offset` itself is forbidden.
    /// Assumes `forbidden` is sorted by `start` and non-overlapping.
    fn clip_to_forbidden(offset: usize, len: usize, forbidden: &[Range<usize>]) -> usize {
        let mut limit = len;
        for r in forbidden {
            if r.start <= offset && offset < r.end {
                return 0; // the source offset itself is unreliable
            }
            if r.start > offset {
                // First forbidden range ahead of `offset` is the binding one
                // (ranges are sorted & non-overlapping): stop before it.
                if r.start - offset < limit {
                    limit = r.start - offset;
                }
                break;
            }
        }
        limit
    }

    /// Core encoder.  When `report` is `Some`, writes the hash table and
    /// opcode trace into the provided writer.  `forbidden` clips COPY sources
    /// (see [`encode_faithful`]); pass `&[]` for the unconstrained encoder.
    fn encode_core(
        old: &[u8],
        new: &[u8],
        forbidden: &[Range<usize>],
        mut report: Option<&mut dyn Write>,
    ) -> Vec<u8> {
        if let Some(ref mut w) = report {
            let _ = writeln!(w, "=== Delta Encoder ===");
            let _ = writeln!(w, "  old size: {} bytes", old.len());
            let _ = writeln!(w, "  new size: {} bytes", new.len());
            let _ = writeln!(w, "  window:   {} bytes", WINDOW_SIZE);
            let _ = writeln!(w, "  min match: {} bytes", MIN_MATCH_LEN);
            let _ = writeln!(w, "  max candidates: {}", MAX_CANDIDATES);
        }

        if new.is_empty() {
            if let Some(ref mut w) = report {
                let _ = writeln!(w, "\n[opcode] END  (empty target)");
            }
            return vec![OP_END];
        }

        // Build a hash table of every WINDOW_SIZE-byte window in `old`.
        let hash_table = build_hash_table(old);

        if let Some(ref mut w) = report {
            let _ = writeln!(w, "\n--- Hash Table ({} unique hashes) ---", hash_table.len());
            let mut entries: Vec<_> = hash_table.iter().collect();
            entries.sort_by_key(|(_, positions)| positions[0]);
            for (hash, positions) in &entries {
                if positions.len() <= 4 {
                    let _ = writeln!(w, "  hash 0x{:08X} -> offsets {:?}", hash, positions);
                } else {
                    let _ = writeln!(
                        w,
                        "  hash 0x{:08X} -> offsets [{}, {}, ... +{} more]",
                        hash,
                        positions[0],
                        positions[1],
                        positions.len() - 2
                    );
                }
            }
            let _ = writeln!(w, "--- End Hash Table ---\n");
        }

        let mut output: Vec<u8> = Vec::new();
        let mut pos: usize = 0;
        let mut add_buf: Vec<u8> = Vec::new();
        let mut opcode_num: usize = 0;

        while pos < new.len() {
            let remaining = new.len() - pos;

            // Try to find a match in `old`
            if remaining >= WINDOW_SIZE {
                let hash = window_hash(&new[pos..pos + WINDOW_SIZE]);
                if let Some(candidates) = hash_table.get(&hash) {
                    let mut best_offset: usize = 0;
                    let mut best_len: usize = 0;
                    for &old_pos in candidates.iter().take(MAX_CANDIDATES) {
                        let raw_len = count_match(old, old_pos, new, pos);
                        // Never let a COPY source enter a forbidden range.
                        let match_len = clip_to_forbidden(old_pos, raw_len, forbidden);
                        if match_len > best_len {
                            best_len = match_len;
                            best_offset = old_pos;
                        }
                    }
                    if best_len >= MIN_MATCH_LEN {
                        // Flush any pending ADD data
                        if let Some(ref mut w) = report {
                            if !add_buf.is_empty() {
                                opcode_num += 1;
                                let _ = writeln!(
                                    w,
                                    "[opcode #{:>3}] ADD   len={:<6} new[{}..{}]",
                                    opcode_num,
                                    add_buf.len(),
                                    pos - add_buf.len(),
                                    pos
                                );
                            }
                        }
                        flush_add(&mut output, &mut add_buf);
                        // Emit COPY
                        if let Some(ref mut w) = report {
                            opcode_num += 1;
                            let _ = writeln!(
                                w,
                                "[opcode #{:>3}] COPY  src_offset={:<6} len={:<6} old[{}..{}] -> new[{}..{}]",
                                opcode_num,
                                best_offset,
                                best_len,
                                best_offset,
                                best_offset + best_len,
                                pos,
                                pos + best_len
                            );
                        }
                        emit_copy(&mut output, best_offset as u32, best_len as u32);
                        pos += best_len;
                        continue;
                    }
                }
            }

            // No match — accumulate byte for ADD
            add_buf.push(new[pos]);
            pos += 1;
        }

        // Flush remaining ADD data
        if let Some(ref mut w) = report {
            if !add_buf.is_empty() {
                opcode_num += 1;
                let _ = writeln!(
                    w,
                    "[opcode #{:>3}] ADD   len={:<6} new[{}..{}]",
                    opcode_num,
                    add_buf.len(),
                    pos - add_buf.len(),
                    pos
                );
            }
        }
        flush_add(&mut output, &mut add_buf);

        // Emit END sentinel
        if let Some(ref mut w) = report {
            opcode_num += 1;
            let _ = writeln!(w, "[opcode #{:>3}] END", opcode_num);
            let _ = writeln!(
                w,
                "\n=== Summary: {} opcodes, {} bytes patch payload (vs {} bytes new) ===",
                opcode_num,
                output.len() + 1,
                new.len()
            );
        }
        output.push(OP_END);

        output
    }

    /// Build a hash table mapping `window_hash` → list of positions in `old`.
    fn build_hash_table(old: &[u8]) -> HashMap<u32, Vec<usize>> {
        let mut table: HashMap<u32, Vec<usize>> = HashMap::new();
        if old.len() < WINDOW_SIZE {
            return table;
        }
        for i in 0..=(old.len() - WINDOW_SIZE) {
            let hash = window_hash(&old[i..i + WINDOW_SIZE]);
            table.entry(hash).or_default().push(i);
        }
        table
    }

    /// Simple hash of a WINDOW_SIZE-byte window.
    fn window_hash(window: &[u8]) -> u32 {
        debug_assert!(window.len() == WINDOW_SIZE);
        let mut h: u32 = 0;
        for &b in window {
            h = h.wrapping_mul(31).wrapping_add(b as u32);
        }
        h
    }

    /// Count how many bytes match starting at `old[old_pos..]` and `new[new_pos..]`.
    fn count_match(old: &[u8], old_pos: usize, new: &[u8], new_pos: usize) -> usize {
        let max = core::cmp::min(old.len() - old_pos, new.len() - new_pos);
        let mut len = 0;
        while len < max && old[old_pos + len] == new[new_pos + len] {
            len += 1;
        }
        len
    }

    /// Flush accumulated ADD bytes into the output stream.
    fn flush_add(output: &mut Vec<u8>, add_buf: &mut Vec<u8>) {
        if add_buf.is_empty() {
            return;
        }
        emit_add(output, add_buf);
        add_buf.clear();
    }

    /// COPY : tag(1) + src_offset(4) + length(4) = 9 bytes.
    fn emit_copy(output: &mut Vec<u8>, src_offset: u32, length: u32) {
        output.push(OP_COPY);
        output.extend_from_slice(&src_offset.to_le_bytes());
        output.extend_from_slice(&length.to_le_bytes());
    }

    /// ADD : tag(1) + length(4) + data(N) = 5+N bytes.
    fn emit_add(output: &mut Vec<u8>, data: &[u8]) {
        output.push(OP_ADD);
        output.extend_from_slice(&(data.len() as u32).to_le_bytes());
        output.extend_from_slice(data);
    }
}

pub use inner::encode;
pub use inner::encode_faithful;
pub use inner::encode_to_report;
