// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `delta_patcher` — COPY/ADD/END binary delta codec for ConceptOS CBF updates.
//!
//! This library provides both the **encoder** (host-side, `std`) and the
//! **decoder** (device-side, `no_std`) for the v1 delta patch format described
//! in `documentation/ConceptOSDeltaBinaryFormat.md`.
//!
//! # Features
//!
//! - `std` — enables the encoder module and `Vec`-based APIs.  Use this on the
//!   host when building `delta_gen`.
//! - (default) — `no_std` core only: format definitions, CRC-32b, and the
//!   streaming decoder.  Use this on the device inside the `update` component.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod decoder;
#[cfg(feature = "std")]
pub mod encoder;
pub mod error;
pub mod format;

// Re-exports for convenience
pub use decoder::{Decoder, DecoderAction};
#[cfg(feature = "std")]
pub use decoder::decode_to_report;
pub use error::DecodeError;
pub use format::{
    crc32b, Crc32State, DeltaHeader, ALGO_COPY_ADD_V1, DELTA_FORMAT_V1, DELTA_HEADER_SIZE,
    DELTA_MAGIC, MAX_OPCODE_HEADER, OP_ADD, OP_COPY, OP_END,
};

#[cfg(feature = "std")]
pub use encoder::encode;
#[cfg(feature = "std")]
pub use encoder::encode_faithful;
#[cfg(feature = "std")]
pub use encoder::encode_to_report;
