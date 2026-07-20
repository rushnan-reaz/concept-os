// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// Errors produced by the opcode decoder (device-side, no_std).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The opcode tag byte is not a recognised v1 opcode.
    UnknownOpcode(u8),
    /// The supplied buffer is too short to contain a complete opcode header.
    /// The caller should accumulate more bytes and retry.
    BufferTooShort,
    /// A COPY instruction references a source region that exceeds the
    /// declared base CBF size.
    CopyOutOfBounds,
    /// The cumulative output length of all opcodes exceeds the declared
    /// reconstructed CBF size.
    OutputOverflow,
    /// The opcode stream ended (all `patch_payload_size` bytes consumed)
    /// without encountering an END opcode.
    MissingEnd,
    /// An END opcode was encountered before all `patch_payload_size` bytes
    /// were consumed.
    PrematureEnd,
    /// An opcode's header (and, for ADD, its declared data) would push the
    /// consumed byte count past the declared `patch_payload_size`.
    OverlongStream,
    /// `advance_add` was called with a length that does not match the pending
    /// `Add` action (or with no `Add` pending).
    AddLengthMismatch,
    /// `next_action` was called while the literal data of a preceding `Add`
    /// action had not yet been consumed via `advance_add`.
    PendingAddData,
}
