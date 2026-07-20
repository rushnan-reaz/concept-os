// Throwaway measurement harness for Step 0.1: run the current `encode` on a
// real recompiled component pair and report compression + COPY/ADD counts.
//
//   cargo run --example measure -- <old.hbf> <new.hbf>

use delta_patcher::format::{OP_ADD, OP_COPY, OP_END};
use delta_patcher::{encode, Decoder, DecoderAction};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: measure <old.hbf> <new.hbf>");
        std::process::exit(2);
    }
    let old = std::fs::read(&args[1]).expect("read old");
    let new = std::fs::read(&args[2]).expect("read new");

    let patch = encode(&old, &new);

    // Parse the patch stream to count opcodes and byte sources.
    let (mut n_copy, mut n_add) = (0usize, 0usize);
    let (mut copy_bytes, mut add_bytes) = (0usize, 0usize);
    let mut p = 0usize;
    while p < patch.len() {
        match patch[p] {
            OP_COPY => {
                let len = u32::from_le_bytes(patch[p + 5..p + 9].try_into().unwrap()) as usize;
                n_copy += 1;
                copy_bytes += len;
                p += 9;
            }
            OP_ADD => {
                let len = u32::from_le_bytes(patch[p + 1..p + 5].try_into().unwrap()) as usize;
                n_add += 1;
                add_bytes += len;
                p += 5 + len;
            }
            OP_END => break,
            other => panic!("bad opcode 0x{other:02x} at {p}"),
        }
    }

    // Roundtrip self-check via the streaming decoder.
    let mut dec = Decoder::new(patch.len() as u32, old.len() as u32, new.len() as u32);
    let mut out = vec![0u8; new.len()];
    let mut pp = 0usize;
    let mut w = 0usize;
    loop {
        let (action, hdr) = dec.next_action(&patch[pp..]).expect("decode");
        match action {
            DecoderAction::Copy { src_offset, len } => {
                let len = len as usize;
                out[w..w + len]
                    .copy_from_slice(&old[src_offset as usize..src_offset as usize + len]);
                w += len;
                pp += hdr;
            }
            DecoderAction::Add { len } => {
                let len = len as usize;
                out[w..w + len].copy_from_slice(&patch[pp + hdr..pp + hdr + len]);
                w += len;
                dec.advance_add(len as u32).expect("advance_add");
                pp += hdr + len;
            }
            DecoderAction::Done => break,
        }
    }
    assert_eq!(out, new, "roundtrip mismatch");

    let pct = 100.0 * (1.0 - patch.len() as f64 / new.len() as f64);
    println!("=== delta measurement (current `encode`, no forbidden set) ===");
    println!("old.hbf:            {} bytes", old.len());
    println!("new.hbf:            {} bytes", new.len());
    println!("patch:              {} bytes", patch.len());
    println!("reduction vs new:   {pct:.1}%");
    println!("COPY ops:           {n_copy}  ({copy_bytes} bytes reconstructed)");
    println!("ADD ops:            {n_add}  ({add_bytes} literal bytes shipped)");
    println!("roundtrip:          OK (decoded == new.hbf)");
}
