//! # rob-core (vendored — `rtp` only)
//!
//! A vendored copy of the AirMatrix `rob-core` crate's `rtp` module, so the
//! loadable program can build the AMT/ROB-shaped RTP header with the engine's
//! own code rather than a re-implementation. Source:
//! `github.com/robmitchellengineering-hub/airmatrix`, branch `task/no-std-engine`,
//! commit `ac71106`, file `crates/rob-core/src/{rtp.rs, lib.rs}`.
//!
//! Only the wire format the program needs is carried here (`rtp` + `Error`).
//! The full crate (aes67, ptp, correction, dedupe, payload, …) lives in the
//! airmatrix repo. When rob-core's `rtp.rs` changes there, re-vendor it.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod rtp;

/// Errors from parsing or building ROB packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The buffer ended before the structure it should contain.
    TooShort,
    /// RTP version was not 2.
    BadVersion,
    /// CSRC lists are not used by ROB.
    CsrcUnsupported,
    /// The header extension is malformed.
    BadExtension,
    /// A field value is outside its allowed range.
    FieldRange,
    /// The output buffer is too small for the encoded data.
    OutputTooSmall,
    /// The payload block table is malformed.
    BadBlocks,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Error::TooShort => "buffer too short",
            Error::BadVersion => "RTP version is not 2",
            Error::CsrcUnsupported => "CSRC lists are not supported",
            Error::BadExtension => "malformed RTP header extension",
            Error::FieldRange => "field value out of range",
            Error::OutputTooSmall => "output buffer too small",
            Error::BadBlocks => "malformed payload block table",
        };
        f.write_str(s)
    }
}

impl core::error::Error for Error {}
