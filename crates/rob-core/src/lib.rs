//! # rob-core
//!
//! The AirMatrix Transport (ROB) wire format and the pure logic around it: RTP
//! headers with the `rob-core` extension, audio payload blocks, the BC10
//! block-companded codec, duplicate detection and timestamp unwrapping.
//!
//! This crate does no I/O and never allocates on its decode paths, so it is safe
//! to call from a real-time audio thread. The normative description is
//! `docs/design/04-protocol-spec-v0.1.md`; if code and spec disagree, fix one
//! of them in the same change.
//!
//! `#![no_std]` + `alloc`: the wire format and its parsers/builders must run on
//! the MorpheOS-Secure Core's loadable programs as well as in the hosted
//! AirMatrix engine, so the crate links against `alloc` rather than `std`. The
//! only `std` surface was `std::error::Error` (now `core::error::Error`) and
//! `std::net::Ipv4Addr` (now `core::net::Ipv4Addr`).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod aes67;
pub mod bc10;
pub mod correction;
pub mod dedupe;
pub mod layer;
pub mod payload;
pub mod pcm;
pub mod ptp;
pub mod resend;
pub mod rtp;
pub mod syncmsg;
pub mod timestamp;

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
