//! The ROB payload: a block table followed by audio blocks.
//!
//! Layout (spec, "Payload: audio blocks"): 1 byte block count, then one 4-byte
//! descriptor per block (type, period offset, length u16 big-endian), then the
//! block bodies in descriptor order.

use crate::Error;

/// Most blocks one packet may carry. Keeps parsing allocation-free.
pub const MAX_BLOCKS: usize = 8;
/// Bytes per block descriptor.
pub const DESCRIPTOR_LEN: usize = 4;

/// Block types (spec table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BlockType {
    Pcm24 = 0x01,
    Pcm16 = 0x02,
    Bc10 = 0x03,
    /// Lossless-compressed PCM24, reserved for v0.2.
    Llc = 0x04,
    /// Tone or ident metadata.
    Test = 0x10,
}

impl BlockType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x01 => BlockType::Pcm24,
            0x02 => BlockType::Pcm16,
            0x03 => BlockType::Bc10,
            0x04 => BlockType::Llc,
            0x10 => BlockType::Test,
            _ => return None,
        })
    }
}

/// One block inside a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block<'a> {
    /// Raw type byte; use [`BlockType::from_u8`]. Unknown types are kept so a
    /// newer sender doesn't break an older receiver.
    pub kind: u8,
    /// 0 = this packet's period, 1 = the previous period, and so on.
    pub period_offset: u8,
    pub data: &'a [u8],
}

/// Blocks parsed from one payload, without allocating.
#[derive(Debug, Clone, Copy)]
pub struct Blocks<'a> {
    count: usize,
    items: [Option<Block<'a>>; MAX_BLOCKS],
}

impl<'a> Blocks<'a> {
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = &Block<'a>> {
        self.items[..self.count].iter().flatten()
    }

    /// The first block of `kind` for the given period offset.
    pub fn find(&self, kind: BlockType, period_offset: u8) -> Option<&Block<'a>> {
        self.iter()
            .find(|b| b.kind == kind as u8 && b.period_offset == period_offset)
    }
}

/// Parses the block table.
pub fn parse(payload: &[u8]) -> Result<Blocks<'_>, Error> {
    let (&count, rest) = payload.split_first().ok_or(Error::TooShort)?;
    let count = usize::from(count);
    if count > MAX_BLOCKS {
        return Err(Error::BadBlocks);
    }
    let table_len = count * DESCRIPTOR_LEN;
    if rest.len() < table_len {
        return Err(Error::TooShort);
    }
    let (table, mut body) = rest.split_at(table_len);
    let mut items = [None; MAX_BLOCKS];
    for (i, d) in table.chunks_exact(DESCRIPTOR_LEN).enumerate() {
        let len = usize::from(u16::from_be_bytes([d[2], d[3]]));
        if body.len() < len {
            return Err(Error::TooShort);
        }
        let (data, next) = body.split_at(len);
        items[i] = Some(Block {
            kind: d[0],
            period_offset: d[1],
            data,
        });
        body = next;
    }
    Ok(Blocks { count, items })
}

/// Builds a payload into a caller-provided buffer.
///
/// Reserve the table with [`PayloadWriter::new`], then append each block with
/// [`PayloadWriter::push_with`], which hands the encoder the remaining space.
pub struct PayloadWriter<'b> {
    buf: &'b mut [u8],
    count: usize,
    max_blocks: usize,
    pos: usize,
}

impl<'b> PayloadWriter<'b> {
    /// Starts a payload that will hold exactly `blocks` blocks.
    pub fn new(buf: &'b mut [u8], blocks: usize) -> Result<Self, Error> {
        if blocks > MAX_BLOCKS {
            return Err(Error::BadBlocks);
        }
        let header = 1 + blocks * DESCRIPTOR_LEN;
        if buf.len() < header {
            return Err(Error::OutputTooSmall);
        }
        buf[0] = blocks as u8;
        Ok(PayloadWriter {
            buf,
            count: 0,
            max_blocks: blocks,
            pos: header,
        })
    }

    /// Appends a block whose body is written by `encode`, which receives the
    /// free space and returns how many bytes it used.
    pub fn push_with(
        &mut self,
        kind: BlockType,
        period_offset: u8,
        encode: impl FnOnce(&mut [u8]) -> Result<usize, Error>,
    ) -> Result<(), Error> {
        if self.count == self.max_blocks {
            return Err(Error::BadBlocks);
        }
        let n = encode(&mut self.buf[self.pos..])?;
        let len = u16::try_from(n).map_err(|_| Error::FieldRange)?;
        let d = 1 + self.count * DESCRIPTOR_LEN;
        self.buf[d] = kind as u8;
        self.buf[d + 1] = period_offset;
        self.buf[d + 2..d + 4].copy_from_slice(&len.to_be_bytes());
        self.pos += n;
        self.count += 1;
        Ok(())
    }

    /// Finishes the payload, returning its length. Fails if fewer blocks were
    /// pushed than reserved.
    pub fn finish(self) -> Result<usize, Error> {
        if self.count != self.max_blocks {
            return Err(Error::BadBlocks);
        }
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bc10, pcm};

    #[test]
    fn wifi_default_layout_matches_spec_size() {
        // Spec "Size budget": stereo, 48 kHz, P = 2 ms.
        let now: Vec<i32> = (0..192).map(|i| i * 1000).collect();
        let prev: Vec<i32> = (0..192).map(|i| -i * 1000).collect();
        let prev2: Vec<i32> = (0..192).map(|i| i * 37).collect();
        let mut buf = [0u8; 1500];
        let mut w = PayloadWriter::new(&mut buf, 3).unwrap();
        w.push_with(BlockType::Pcm24, 0, |o| pcm::encode_pcm24(&now, o))
            .unwrap();
        w.push_with(BlockType::Pcm16, 1, |o| pcm::encode_pcm16(&prev, o))
            .unwrap();
        w.push_with(BlockType::Bc10, 2, |o| bc10::encode(&prev2, 2, 48, o))
            .unwrap();
        let len = w.finish().unwrap();
        assert_eq!(len, 576 + 384 + 244 + 13, "spec: 1,217-byte payload");

        let blocks = parse(&buf[..len]).unwrap();
        assert_eq!(blocks.len(), 3);
        let b = blocks.find(BlockType::Pcm16, 1).unwrap();
        let mut out = [0i32; 192];
        pcm::decode_pcm16(b.data, &mut out).unwrap();
        assert_eq!(out[5], (prev[5] >> 8) << 8);
        assert!(blocks.find(BlockType::Pcm24, 1).is_none());
    }

    #[test]
    fn rejects_truncated_tables() {
        assert_eq!(parse(&[]).unwrap_err(), Error::TooShort);
        assert_eq!(parse(&[1, 1, 0, 0]).unwrap_err(), Error::TooShort);
        assert_eq!(parse(&[1, 1, 0, 0, 5, 1, 2]).unwrap_err(), Error::TooShort);
        assert_eq!(parse(&[9]).unwrap_err(), Error::BadBlocks);
    }

    #[test]
    fn unknown_block_types_survive_parsing() {
        let p = [1u8, 0x7E, 0, 0, 2, 0xAA, 0xBB];
        let blocks = parse(&p).unwrap();
        let b = blocks.iter().next().unwrap();
        assert_eq!(b.kind, 0x7E);
        assert_eq!(BlockType::from_u8(b.kind), None);
        assert_eq!(b.data, &[0xAA, 0xBB]);
    }

    #[test]
    fn writer_enforces_block_count() {
        let mut buf = [0u8; 64];
        let w = PayloadWriter::new(&mut buf, 2).unwrap();
        assert_eq!(w.finish().unwrap_err(), Error::BadBlocks);
    }
}
