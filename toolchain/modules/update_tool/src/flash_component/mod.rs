// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

mod messages;

use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use hbf_rs::HbfFile;
use pbr::ProgressBar;
use std::{io::Stdout, path::PathBuf};

use self::messages::*;
use crate::common_messages::*;
use crate::utils::*;

pub fn flash_component(
    channel_in_consumer: Receiver<u8>,
    channel_out_producer: Sender<Vec<u8>>,
    hbf_file: String,
    verbose: bool,
) {
    if verbose {
        println!("---> Flashing Component");
    }
    // First, check the hbf path
    let hbf_path = PathBuf::from(hbf_file.clone());
    if !hbf_path.exists() {
        panic!("Cannot find the HBF at '{}'", hbf_file);
    }
    // Read the whole hbf in memory (surely small for a PC)
    let hbf_bytes =
        std::fs::read(hbf_path).expect(&format!("Cannot open the HBF at '{}'", hbf_file));
    // Parse the hbf
    let hbf_result = hbf_rs::parse_hbf(&hbf_bytes);
    if hbf_result.is_err() {
        match hbf_result.unwrap_err() {
            hbf_rs::Error::BufferTooShort | hbf_rs::Error::InvalidMagic => {
                panic!("HBF file not valid!")
            }
            hbf_rs::Error::UnsupportedVersion => {
                panic!("HBF version still not supported by the tool")
            }
        }
    }
    let hbf = hbf_result.unwrap();

    // Detect a Delta HBF by the IS_DELTA flag (bit 1 of Component Flags). We
    // read the raw flag word because hbf_rs' ComponentFlags does not define
    // IS_DELTA and would truncate it. A Delta HBF carries a CRC-32b trailer
    // (not the XOR checksum), so the standard `hbf.validate()` must be skipped.
    let flags_offset = hbf_rs::HBF_HEADER_MIN_SIZE + 2;
    let component_flags = u16::from_le_bytes([hbf_bytes[flags_offset], hbf_bytes[flags_offset + 1]]);
    const IS_DELTA_BIT: u16 = 1 << 1;
    if component_flags & IS_DELTA_BIT != 0 {
        flash_delta_component(
            &channel_in_consumer,
            &channel_out_producer,
            &hbf_bytes,
            &hbf,
            verbose,
        );
        return;
    }

    // Validate hbf
    if !hbf.validate() {
        panic!("HBF file integrity test failed!");
    }
    // If verbose, print some info
    if verbose {
        println!("\n\tComponent ID: {}", hbf.header_base().component_id());
        println!(
            "\tComponent Version: {}",
            hbf.header_base().component_version()
        );
        println!("\tRequired Flash Size: {}", hbf.header_base().total_size());
        println!(
            "\tRequired SRAM Size: {}",
            hbf.header_main().component_min_ram()
        );
    }
    // Send hello
    println!("");
    let mut progress = ProgressBar::new((hbf.header_base().total_size() + 4) as u64);
    progress.show_speed = false;
    progress.show_counter = false;
    progress.show_time_left = false;
    progress.set_width(Some(80));
    begin_communication(
        &channel_in_consumer,
        &channel_out_producer,
        &hbf,
        &mut progress,
        verbose,
    );
}

fn begin_communication(
    channel_in_consumer: &Receiver<u8>,
    channel_out_producer: &Sender<Vec<u8>>,
    hbf: &dyn HbfFile,
    progress: &mut ProgressBar<Stdout>,
    verbose: bool,
) {
    // Send hello message
    progress.message("Connection Setup   ");
    let hello_msg = HelloMessage::new(OperationType::ComponentUpdate);
    channel_flush_read(channel_in_consumer);
    channel_write(channel_out_producer, &hello_msg.get_raw());
    // Read hello response
    let mut buff: [u8; HelloResponseMessage::get_size()] = [0x00; HelloResponseMessage::get_size()];
    channel_read(channel_in_consumer, &mut buff);
    // Validate hello response
    let hello_response = HelloResponseMessage::from(&buff);
    if hello_response.is_err() {
        eprintln!("Wrong response from device at HELLO");
        return;
    }
    if verbose {
        println!("Got HELLO!");
    }
    progress.inc();
    // Wait for header request
    let mut buff: [u8; 1] = [0x00; 1];
    //flush_read(serial);
    channel_read(channel_in_consumer, &mut buff);
    if buff[0] != ComponentUpdateCommand::SendComponentFixedHeader as u8 {
        eprintln!(
            "Unexpected response from device at first step (Fixed Header): {:?}",
            MessageError::from(buff[0])
        );
        return;
    }
    send_fixed_header(channel_in_consumer, channel_out_producer, hbf, progress, verbose);
}

fn send_fixed_header(
    channel_in_consumer: &Receiver<u8>,
    channel_out_producer: &Sender<Vec<u8>>,
    hbf: &dyn HbfFile,
    progress: &mut ProgressBar<Stdout>,
    verbose: bool,
) {
    if verbose {
        println!("--> Send Fixed Header");
    }
    progress.inc();
    // Send fixed header
    let base_header_raw = hbf.header_base().get_raw();
    let main_header_raw = hbf.header_main().get_raw();
    // Combine in a single packet
    let mut out_buff = Vec::<u8>::new();
    out_buff.extend_from_slice(base_header_raw);
    out_buff.extend_from_slice(main_header_raw);
    // Construct packet and send
    progress.message("Header   ");
    let fixed_header_msg = FixedHeaderMessage::new(&out_buff);
    channel_write(channel_out_producer, &fixed_header_msg.get_raw());
    // Update progress
    progress.add((out_buff.len() - 1) as u64);
    // Wait for variable header request
    let mut buff: [u8; 1] = [0x00; 1];
    channel_read(channel_in_consumer, &mut buff);
    if buff[0] != ComponentUpdateCommand::SendComponentVariableHeader as u8 {
        eprintln!(
            "Unexpected response from device at second step (Variable Header): {:?}",
            MessageError::from(buff[0])
        );
        return;
    }
    send_variable_header(channel_in_consumer, channel_out_producer, hbf, progress, verbose);
}

fn send_variable_header(
    channel_in_consumer: &Receiver<u8>,
    channel_out_producer: &Sender<Vec<u8>>,
    hbf: &dyn HbfFile,
    progress: &mut ProgressBar<Stdout>,
    verbose: bool,
) {
    if verbose {
        println!("--> Send Variable Header");
    }
    progress.inc();
    // Generate bytes
    let vhb = extract_variable_header(hbf);
    // Start sending
    let mut pkt = RawPacket::new(&vhb);
    loop {
        let mut buff: [u8; 1] = [0x00; 1];
        // Wait for next request
        channel_read(channel_in_consumer, &mut buff);

        if buff[0] == ComponentUpdateCommand::SendComponentPayload as u8 {
            // Check we actually finished sending the variable header
            if pkt.get_next_fragment().is_some() {
                eprintln!("Still some header to be send!");
                return;
            }
            break; // Everything okay
        } else if buff[0] != ComponentUpdateCommand::SendNextFragment as u8 {
            eprintln!(
                "Unexpected response from device at third step (Variable Header): {:?}",
                MessageError::from(buff[0])
            );
            return;
        }
        // Send fragment
        progress.message(&format!(
            "Header Fragment {}/{}   ",
            pkt.get_next_fragment_number().unwrap(),
            pkt.get_total_fragments()
        ));
        //println!("\tSending Fragment {}/{}", pkt.get_next_fragment_number().unwrap(), pkt.get_total_fragments());
        let fragment_data = pkt.get_next_fragment().unwrap();
        channel_write(channel_out_producer, &fragment_data);
        // Update progress
        progress.add((fragment_data.len() - 1) as u64);
    }
    send_payload(channel_in_consumer, channel_out_producer, hbf, progress, verbose);
}

fn send_payload(
    channel_in_consumer: &Receiver<u8>,
    channel_out_producer: &Sender<Vec<u8>>,
    hbf: &dyn HbfFile,
    progress: &mut ProgressBar<Stdout>,
    verbose: bool,
) {
    if verbose {
        println!("--> Send Payload");
    }
    progress.inc();
    // -------> Sending Payload
    // Get bytes
    let mut payload_bytes = Vec::<u8>::new();
    payload_bytes.extend_from_slice(hbf.read_only_section().content());
    if hbf.data_section().is_some() {
        payload_bytes.extend_from_slice(hbf.data_section().unwrap().content());
    }
    // Generate packet
    let mut pkt = RawPacket::new(&payload_bytes);
    loop {
        // Wait for next request
        let mut buff: [u8; 1] = [0x00; 1];
        channel_read(channel_in_consumer, &mut buff);

        if buff[0] == ComponentUpdateCommand::SendComponentTrailer as u8 {
            // Check we actually finished sending the variable header
            if pkt.get_next_fragment().is_some() {
                eprintln!("Still some payload to be send!");
                return;
            }
            break;
        } else if buff[0] != ComponentUpdateCommand::SendNextFragment as u8 {
            eprintln!(
                "Unexpected response from device at fourth step (Payload) {:?}",
                MessageError::from(buff[0])
            );
            return;
        }
        // Send fragment
        //println!("\tSending Fragment {}/{}", pkt.get_next_fragment_number().unwrap(), pkt.get_total_fragments());
        progress.message(&format!(
            "Payload Fragment {}/{}   ",
            pkt.get_next_fragment_number().unwrap(),
            pkt.get_total_fragments()
        ));
        let fragment_data = pkt.get_next_fragment().unwrap();
        channel_write(channel_out_producer, &fragment_data);
        // Update progress
        progress.add((fragment_data.len() - 1) as u64);
    }
    send_trailer(channel_in_consumer, channel_out_producer, hbf, progress, verbose);
}

fn send_trailer(
    channel_in_consumer: &Receiver<u8>,
    channel_out_producer: &Sender<Vec<u8>>,
    hbf: &dyn HbfFile,
    progress: &mut ProgressBar<Stdout>,
    verbose: bool,
) {
    if verbose {
        println!("--> Send Trailer");
    }
    progress.inc();
    // -------> Sending Payload
    // Get bytes
    let mut checksum_bytes = Vec::<u8>::new();
    checksum_bytes.extend_from_slice(&hbf.trailer().checksum().to_le_bytes());
    // Send data
    progress.message("Checksum   ");
    channel_write(channel_out_producer, &checksum_bytes);

    // Wait for confirmation
    let mut buff: [u8; 1] = [0x00; 1];
    channel_read(channel_in_consumer, &mut buff);
    if buff[0] != ComponentUpdateResponse::Success as u8 {
        eprintln!(
            "Unexpected response from device at final step: {:?}",
            MessageError::from(buff[0])
        );
        return;
    }
    progress.finish();

    println!("\nSuccess!");
}

// ---------------------------------------------------------------------------
//  Delta HBF serving path (§4d)
// ---------------------------------------------------------------------------
//
// Delta HBF layout (see ConceptOSDeltaBinaryFormat.md / delta_gen):
//   [ fixed header (IS_DELTA set) ][ 32-byte Delta Header ][ patch ][ CRC-32b ]
//
// Device request sequence on the delta path:
//   0x01 (fixed header) -> 0xB0 (delta header) -> 0xA0... (patch fragments)
//   -> optional 0x04 (delta trailer) -> Success (0xFF).

fn flash_delta_component(
    channel_in_consumer: &Receiver<u8>,
    channel_out_producer: &Sender<Vec<u8>>,
    hbf_bytes: &[u8],
    hbf: &dyn HbfFile,
    verbose: bool,
) {
    use delta_patcher::DELTA_HEADER_SIZE;

    let fixed_len = hbf_rs::FIXED_HEADER_SIZE;
    let delta_hdr_start = fixed_len;
    let delta_hdr_end = delta_hdr_start + DELTA_HEADER_SIZE;
    if hbf_bytes.len() < delta_hdr_end + 4 {
        panic!("Delta HBF too short (fixed header + delta header + trailer)");
    }
    // Patch payload size lives at offset 0x18 within the Delta Header.
    let patch_size = u32::from_le_bytes(
        hbf_bytes[delta_hdr_start + 0x18..delta_hdr_start + 0x1C]
            .try_into()
            .unwrap(),
    ) as usize;
    let patch_start = delta_hdr_end;
    let patch_end = patch_start + patch_size;
    if patch_end + 4 > hbf_bytes.len() {
        panic!(
            "Delta HBF inconsistent: patch_size={} overruns file (len={})",
            patch_size,
            hbf_bytes.len()
        );
    }

    let fixed_header = &hbf_bytes[0..fixed_len];
    let delta_header = &hbf_bytes[delta_hdr_start..delta_hdr_end];
    let patch = &hbf_bytes[patch_start..patch_end];
    let delta_trailer = &hbf_bytes[hbf_bytes.len() - 4..];

    if verbose {
        println!("---> Flashing Delta Component");
        println!("\tComponent ID: {}", hbf.header_base().component_id());
        println!("\tComponent Version: {}", hbf.header_base().component_version());
        println!("\tPatch payload: {} bytes", patch_size);
        println!("\tDelta HBF total: {} bytes", hbf_bytes.len());
    }

    let mut progress = ProgressBar::new((fixed_len + DELTA_HEADER_SIZE + patch_size + 4) as u64);
    progress.show_speed = false;
    progress.show_counter = false;
    progress.show_time_left = false;
    progress.set_width(Some(80));

    // --- Hello ---------------------------------------------------------------
    progress.message("Connection Setup   ");
    let hello_msg = HelloMessage::new(OperationType::ComponentUpdate);
    channel_flush_read(channel_in_consumer);
    channel_write(channel_out_producer, &hello_msg.get_raw());
    let mut buff: [u8; HelloResponseMessage::get_size()] = [0x00; HelloResponseMessage::get_size()];
    channel_read(channel_in_consumer, &mut buff);
    if HelloResponseMessage::from(&buff).is_err() {
        eprintln!("Wrong response from device at HELLO");
        return;
    }
    progress.inc();

    // --- Fixed header (0x01) --------------------------------------------------
    let mut req: [u8; 1] = [0x00; 1];
    channel_read(channel_in_consumer, &mut req);
    if req[0] != ComponentUpdateCommand::SendComponentFixedHeader as u8 {
        eprintln!("Unexpected response before fixed header: {:?}", MessageError::from(req[0]));
        return;
    }
    progress.message("Header   ");
    channel_write(channel_out_producer, &FixedHeaderMessage::new(fixed_header).get_raw());
    progress.add((fixed_len - 1) as u64);

    // --- Delta header (0xB0) --------------------------------------------------
    channel_read(channel_in_consumer, &mut req);
    if req[0] != ComponentUpdateCommand::SendDeltaHeader as u8 {
        eprintln!("Unexpected response before delta header: {:?}", MessageError::from(req[0]));
        return;
    }
    progress.message("Delta Header   ");
    channel_write(channel_out_producer, &RawCrc8Message::new(delta_header).get_raw());
    progress.add(DELTA_HEADER_SIZE as u64);

    // --- Patch payload (0xA0 fragments) + optional trailer (0x04) -------------
    let mut pkt = RawPacket::new(patch);
    loop {
        channel_read(channel_in_consumer, &mut req);
        match req[0] {
            x if x == ComponentUpdateCommand::SendNextFragment as u8 => {
                // Read these before get_next_fragment() advances the packet's
                // internal position, mirroring the full-component path above.
                let frag_num = pkt.get_next_fragment_number();
                let frag_total = pkt.get_total_fragments();
                match pkt.get_next_fragment() {
                    Some(fragment) => {
                        progress.message(&format!(
                            "Patch Fragment {}/{}   ",
                            frag_num.unwrap_or(0),
                            frag_total
                        ));
                        let data_len = fragment.len() - 1;
                        channel_write(channel_out_producer, &fragment);
                        progress.add(data_len as u64);
                    }
                    None => {
                        eprintln!("Device requested more patch than available");
                        return;
                    }
                }
            }
            x if x == ComponentUpdateCommand::SendComponentTrailer as u8 => {
                progress.message("Delta Trailer   ");
                channel_write(channel_out_producer, &delta_trailer.to_vec());
                progress.add(4);
            }
            x if x == ComponentUpdateResponse::Success as u8 => {
                progress.finish();
                println!("\nSuccess!");
                return;
            }
            other => {
                eprintln!("Unexpected response during patch transfer: {:?}", MessageError::from(other));
                return;
            }
        }
    }
}

fn extract_variable_header(hbf: &dyn HbfFile) -> Vec<u8> {
    let mut buffer = Vec::<u8>::new();
    // Start with regions
    for r in hbf.region_iter() {
        let raw_data = r.get_raw();
        buffer.extend_from_slice(raw_data);
    }
    // Next append interrupts
    for i in hbf.interrupt_iter() {
        let raw_data = i.get_raw();
        buffer.extend_from_slice(raw_data);
    }
    // Next append relocations
    for r in hbf.relocation_iter() {
        let raw_data = r.get_raw();
        buffer.extend_from_slice(raw_data);
    }
    // Next append dependencies
    for d in hbf.dependency_iter() {
        let raw_data = d.get_raw();
        buffer.extend_from_slice(raw_data);
    }
    // Then append padding bytes
    for _ in 0..hbf.header_base().padding_bytes() {
        buffer.extend([0xFF]);
    }
    buffer
}
