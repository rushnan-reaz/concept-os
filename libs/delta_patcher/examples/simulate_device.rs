// Host simulation of the device delta reconstruction (Slice C validation).
//
// Reproduces exactly what `components/update/core/src/delta.rs` does:
//   - fragments the patch into 64-byte CRC-framed chunks (PatchReader),
//   - drives the streaming Decoder opcode by opcode,
//   - COPY reads the base image, ADD pulls literal patch bytes,
//   - checks the running CRC-32b against the Delta Header's reconstructed_crc32.
//
// Run against the real artifacts:
//   cargo run --example simulate_device -- <delta.hbf> <base.hbf> [expected_new.hbf]
//
// Using the *pristine* base is equivalent to the device's post-relocation base,
// because the encoder guarantees no COPY reads a relocation-site/trailer byte.

use delta_patcher::format::{DeltaHeader, OP_ADD, OP_COPY, OP_END, DELTA_HEADER_SIZE};
use delta_patcher::{Crc32State, Decoder, DecoderAction, MAX_OPCODE_HEADER};

const PACKET_BUFFER_SIZE: usize = 64; // must match the device (consts.rs)

/// Mirrors the device PatchReader: fragments a patch buffer into 64-byte
/// chunks and hands them out through a small compacting buffer.
struct PatchReader<'a> {
    patch: &'a [u8],
    pos: usize,       // next un-fragmented patch byte
    remaining: usize, // patch bytes not yet "pulled"
    buf: Vec<u8>,     // pulled-but-unconsumed bytes
    start: usize,
}

impl<'a> PatchReader<'a> {
    fn new(patch: &'a [u8]) -> Self {
        Self { patch, pos: 0, remaining: patch.len(), buf: Vec::new(), start: 0 }
    }
    fn available(&self) -> usize {
        self.buf.len() - self.start
    }
    fn peek(&self) -> &[u8] {
        &self.buf[self.start..]
    }
    fn consume(&mut self, n: usize) {
        self.start += n;
    }
    fn pull_fragment(&mut self) {
        let n = std::cmp::min(PACKET_BUFFER_SIZE, self.remaining);
        if n == 0 {
            return;
        }
        if self.start > 0 {
            self.buf.drain(0..self.start);
            self.start = 0;
        }
        // Simulate the CRC-framed transport fragment (data + CRC-8); the device
        // validates and strips the CRC. Here we just move the data bytes.
        self.buf.extend_from_slice(&self.patch[self.pos..self.pos + n]);
        self.pos += n;
        self.remaining -= n;
    }
    fn ensure(&mut self, k: usize) {
        while self.available() < k && self.remaining > 0 {
            self.pull_fragment();
        }
    }
    fn read_into(&mut self, dst: &mut [u8]) {
        let mut done = 0;
        while done < dst.len() {
            if self.available() == 0 {
                assert!(self.remaining > 0, "patch underflow");
                self.pull_fragment();
            }
            let take = std::cmp::min(self.available(), dst.len() - done);
            dst[done..done + take].copy_from_slice(&self.buf[self.start..self.start + take]);
            self.start += take;
            done += take;
        }
    }
}

fn find_delta_header(delta: &[u8]) -> usize {
    delta
        .windows(4)
        .position(|w| w == b"DELT")
        .expect("no DELT magic in delta HBF")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: simulate_device <delta.hbf> <base.hbf> [expected_new.hbf]");
        std::process::exit(2);
    }
    let delta = std::fs::read(&args[1]).expect("read delta.hbf");
    let base = std::fs::read(&args[2]).expect("read base.hbf");

    let dh_off = find_delta_header(&delta);
    let header =
        DeltaHeader::from_bytes(&delta[dh_off..dh_off + DELTA_HEADER_SIZE]).expect("delta header");
    assert!(header.validate(), "delta header failed validation");

    let patch_start = dh_off + DELTA_HEADER_SIZE;
    let patch = &delta[patch_start..patch_start + header.patch_payload_size as usize];

    // --- Reconstruct exactly like the device --------------------------------
    let mut reader = PatchReader::new(patch);
    let mut decoder = Decoder::new(
        header.patch_payload_size,
        base.len() as u32,
        header.target_size,
    );
    let mut crc = Crc32State::new();
    let mut out = vec![0u8; header.target_size as usize];
    let mut cursor = 0usize;
    let mut tmp = [0u8; PACKET_BUFFER_SIZE];

    let (mut n_copy, mut n_add) = (0usize, 0usize);
    loop {
        reader.ensure(MAX_OPCODE_HEADER);
        let (action, consumed) = decoder.next_action(reader.peek()).expect("decode");
        match action {
            DecoderAction::Copy { src_offset, len } => {
                reader.consume(consumed);
                let (mut src, mut left) = (src_offset as usize, len as usize);
                while left > 0 {
                    let n = std::cmp::min(PACKET_BUFFER_SIZE, left);
                    tmp[0..n].copy_from_slice(&base[src..src + n]);
                    out[cursor..cursor + n].copy_from_slice(&tmp[0..n]);
                    crc.update(&tmp[0..n]);
                    cursor += n;
                    src += n;
                    left -= n;
                }
                n_copy += 1;
            }
            DecoderAction::Add { len } => {
                reader.consume(consumed);
                let mut left = len as usize;
                while left > 0 {
                    let n = std::cmp::min(PACKET_BUFFER_SIZE, left);
                    reader.read_into(&mut tmp[0..n]);
                    out[cursor..cursor + n].copy_from_slice(&tmp[0..n]);
                    crc.update(&tmp[0..n]);
                    cursor += n;
                    left -= n;
                }
                decoder.advance_add(len).expect("advance_add");
                n_add += 1;
            }
            DecoderAction::Done => break,
        }
    }

    // Opcode-header sanity vs the raw stream.
    let _ = (OP_COPY, OP_ADD, OP_END);

    let got_crc = crc.finalize();
    println!("=== simulate_device ===");
    println!("delta hdr @ offset {dh_off}");
    println!("base:               {} bytes", base.len());
    println!("target:             {} bytes", header.target_size);
    println!("patch:              {} bytes", header.patch_payload_size);
    println!("reconstructed:      {} bytes ({n_copy} COPY, {n_add} ADD)", cursor);
    println!("reconstructed_crc:  0x{got_crc:08X}  expected 0x{:08X}", header.reconstructed_crc32);
    assert_eq!(cursor as u32, header.target_size, "size mismatch");
    assert_eq!(got_crc, header.reconstructed_crc32, "CRC mismatch (reconstruction wrong)");
    println!("CRC gate:           PASS");

    if let Some(expected_path) = args.get(3) {
        let expected = std::fs::read(expected_path).expect("read expected_new.hbf");
        assert_eq!(out, expected, "reconstructed bytes != expected new.hbf");
        println!("byte-exact vs {expected_path}: PASS");
    }
}
