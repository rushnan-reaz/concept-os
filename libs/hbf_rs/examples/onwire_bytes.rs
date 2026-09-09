// Temporary, throwaway example: reproduces the exact on-wire byte accounting
// for the full-component update path (see toolchain/modules/update_tool/src/
// flash_component/mod.rs) directly against a real HBF file, so Chapter 4's
// on-wire byte table can be recomputed for the size-matched bthermo build
// without needing a live device.
use hbf_rs::HbfFile;
use std::env;

const PACKET_BUFFER_SIZE: usize = 64;
const MUX: usize = 9; // 4B preamble + 2B channel id + 2B length + 1B CRC-8

fn frag_count_and_last(len: usize) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    let full = len / PACKET_BUFFER_SIZE;
    let rem = len % PACKET_BUFFER_SIZE;
    if rem == 0 {
        (full, PACKET_BUFFER_SIZE)
    } else {
        (full + 1, rem)
    }
}

fn main() {
    let path = env::args().nth(1).expect("usage: onwire_bytes <hbf-file>");
    let bytes = std::fs::read(&path).expect("read hbf");
    let hbf = hbf_rs::parse_hbf(&bytes).expect("parse hbf");

    // --- variable header bytes (regions + interrupts + relocations + deps + padding)
    let mut vhb_len = 0usize;
    for r in hbf.region_iter() {
        vhb_len += r.get_raw().len();
    }
    for i in hbf.interrupt_iter() {
        vhb_len += i.get_raw().len();
    }
    for r in hbf.relocation_iter() {
        vhb_len += r.get_raw().len();
    }
    for d in hbf.dependency_iter() {
        vhb_len += d.get_raw().len();
    }
    vhb_len += hbf.header_base().padding_bytes() as usize;

    // --- payload bytes (read-only section + optional data section)
    let mut payload_len = hbf.read_only_section().content().len();
    if let Some(ds) = hbf.data_section() {
        payload_len += ds.content().len();
    }

    let fixed_header_raw = hbf_rs::FIXED_HEADER_SIZE; // base+main, no CRC
    let body_len = vhb_len + payload_len;
    let full_hbf_len = bytes.len();

    println!("file: {}", path);
    println!("full HBF size: {} B", full_hbf_len);
    println!("fixed header (raw, no crc): {} B", fixed_header_raw);
    println!("variable header bytes: {} B", vhb_len);
    println!("payload bytes: {} B", payload_len);
    println!("body (var header + payload): {} B", body_len);
    println!("trailer: {} B", full_hbf_len - fixed_header_raw - body_len);

    // --- fragment accounting: variable header and payload are two
    // *independent* RawPacket streams (see extract_variable_header /
    // send_variable_header / send_payload in flash_component/mod.rs)
    let (vh_frags, vh_last) = frag_count_and_last(vhb_len);
    let (pl_frags, pl_last) = frag_count_and_last(payload_len);
    let total_frags = vh_frags + pl_frags;
    println!(
        "variable-header fragments: {} (last = {} B)",
        vh_frags, vh_last
    );
    println!("payload fragments: {} (last = {} B)", pl_frags, pl_last);
    println!("total 0xA0 fragments: {}", total_frags);

    // --- device -> host ---
    // Hello is host-initiated (channel_write from the HOST comes first,
    // unprompted), so HelloMessage(2B) is host->device and
    // HelloResponseMessage(7B) is device->host -- the OPPOSITE of every
    // later request/response pair, where the device sends the 1-byte
    // command and the host answers with data. Confirmed by re-reading
    // begin_communication()/flash_delta_component() in
    // flash_component/mod.rs: channel_out_producer is the host's outgoing
    // channel, and HelloMessage is the first thing written to it, with no
    // preceding device request.
    let d2h_requests = 1 /*hello response*/ + 1 /*0x01*/ + 1 /*0x02*/ + vh_frags + 1 /*0x03*/ + pl_frags + 1 /*0x04*/;
    let d2h_bytes_no_success =
        MUX + 7 /*hello response*/
        + MUX + 1 /*0x01*/
        + MUX + 1 /*0x02*/
        + vh_frags * (MUX + 1)
        + MUX + 1 /*0x03*/
        + pl_frags * (MUX + 1)
        + MUX + 1 /*0x04*/;
    let success_bytes = MUX + 1;

    // --- host -> device ---
    // HelloMessage(2B, host-initiated) + FixedHeaderMessage(60+1crc=61B)
    // + vh fragments (64+1crc=65B full, last = vh_last+1crc)
    // + pl fragments (64+1crc=65B full, last = pl_last+1crc)
    // + trailer checksum (4B, no crc appended in code)
    let h2d_bytes = MUX + 2 /*hello request*/
        + MUX + (fixed_header_raw + 1) /*fixed header*/
        + if vh_frags > 0 {
            (vh_frags - 1) * (MUX + PACKET_BUFFER_SIZE + 1) + (MUX + vh_last + 1)
        } else {
            0
        }
        + if pl_frags > 0 {
            (pl_frags - 1) * (MUX + PACKET_BUFFER_SIZE + 1) + (MUX + pl_last + 1)
        } else {
            0
        }
        + MUX + 4 /*trailer checksum, no crc*/;

    println!();
    println!("--- on-wire byte accounting ---");
    println!("pull-request packets (device->host, excl. Success): {}", d2h_requests);
    println!(
        "device->host bytes (incl. mux, excl. Success): {} B",
        d2h_bytes_no_success
    );
    println!(
        "device->host bytes (incl. mux, incl. Success): {} B",
        d2h_bytes_no_success + success_bytes
    );
    println!("host->device bytes (incl. mux): {} B", h2d_bytes);
    println!(
        "total on-wire bytes (excl. Success): {} B",
        d2h_bytes_no_success + h2d_bytes
    );
    println!(
        "total on-wire bytes (incl. Success): {} B",
        d2h_bytes_no_success + success_bytes + h2d_bytes
    );

    if let Some(delta_path) = env::args().nth(2) {
        let delta_bytes = std::fs::read(&delta_path).expect("read delta hbf");
        delta_calc(&delta_bytes);
    }
}

// ---------------------------------------------------------------------
// Delta-path calculator, used only to calibrate/validate the formula
// above against the already-known-correct delta figures (1833/1929 B,
// 29 fragments, 34 requests, 346/2259/2605 B) before trusting it on the
// full-component (baseline) path.
// ---------------------------------------------------------------------
#[allow(dead_code)]
fn delta_calc(bytes: &[u8]) {
    const DELTA_HEADER_SIZE: usize = 32;
    let fixed_len = hbf_rs::FIXED_HEADER_SIZE;
    let delta_hdr_start = fixed_len;
    let delta_hdr_end = delta_hdr_start + DELTA_HEADER_SIZE;
    let patch_size = u32::from_le_bytes(
        bytes[delta_hdr_start + 0x18..delta_hdr_start + 0x1C]
            .try_into()
            .unwrap(),
    ) as usize;
    let (patch_frags, patch_last) = frag_count_and_last(patch_size);

    println!();
    println!("=== delta-path calibration ===");
    println!("patch_size: {} B", patch_size);
    println!("delta HBF total: {} B", bytes.len());
    println!("patch fragments: {} (last = {} B)", patch_frags, patch_last);

    // device -> host: HelloResponse(7) + 0x01(1) + 0xB0(1) + N*0xA0(1) + 0x04(1) [+ Success(1)]
    let d2h_no_success =
        MUX + 7 + MUX + 1 + MUX + 1 + patch_frags * (MUX + 1) + MUX + 1;
    let success = MUX + 1;

    // host -> device: Hello(2, host-initiated) + FixedHeader(60+1) + DeltaHeader(32+1)
    // + fragments (64+1 full, last=patch_last+1) + trailer(4, no crc)
    let h2d = MUX + 2
        + MUX + (fixed_len + 1)
        + MUX + (DELTA_HEADER_SIZE + 1)
        + if patch_frags > 0 {
            (patch_frags - 1) * (MUX + PACKET_BUFFER_SIZE + 1) + (MUX + patch_last + 1)
        } else {
            0
        }
        + MUX + 4;

    println!("pull-requests excl. Success: {}", 4 + patch_frags);
    println!("pull-requests incl. Success: {}", 5 + patch_frags);
    println!("device->host excl. Success: {} B", d2h_no_success);
    println!("device->host incl. Success: {} B", d2h_no_success + success);
    println!("host->device: {} B", h2d);
    println!("total excl. Success: {} B", d2h_no_success + h2d);
    println!("total incl. Success: {} B", d2h_no_success + success + h2d);
}
