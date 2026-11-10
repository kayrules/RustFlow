//! Just enough of a Matroska reader to pull H.264 out of GStreamer's
//! `matroskamux streamable=true`: the codec setup and each frame, as soon
//! as it is complete (Matroska gives every block its size up front).

use std::io::{self, Read};

const SEGMENT: u32 = 0x1853_8067;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const CODEC_PRIVATE: u32 = 0x63A2;
const CLUSTER: u32 = 0x1F43_B675;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;

/// Elements whose children we read; every other element is skipped.
const CONTAINERS: [u32; 5] = [SEGMENT, TRACKS, TRACK_ENTRY, CLUSTER, BLOCK_GROUP];

/// Frames bigger than this are treated as a corrupt stream.
const MAX_ELEMENT: u64 = 64 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub enum Item {
    /// The track's `avcC` record.
    CodecPrivate(Vec<u8>),
    Frame { key: bool, data: Vec<u8> },
}

pub struct Reader<R> {
    r: R,
}

impl<R: Read> Reader<R> {
    pub fn new(r: R) -> Self {
        Self { r }
    }

    /// The next codec setup or frame; `None` at the end of the stream.
    pub fn next_item(&mut self) -> io::Result<Option<Item>> {
        loop {
            let Some(id) = self.read_id()? else { return Ok(None) };
            let size = self.read_size()?;
            if CONTAINERS.contains(&id) {
                continue; // read its children as the next elements
            }
            let Some(size) = size else {
                return Err(bad(format!("element {id:#x} has unknown size")));
            };
            if size > MAX_ELEMENT {
                return Err(bad(format!("element {id:#x} is {size} bytes")));
            }
            match id {
                CODEC_PRIVATE => return Ok(Some(Item::CodecPrivate(self.read_vec(size)?))),
                SIMPLE_BLOCK | BLOCK => {
                    if let Some(item) = block(self.read_vec(size)?, id == SIMPLE_BLOCK)? {
                        return Ok(Some(item));
                    }
                }
                _ => {
                    io::copy(&mut (&mut self.r).take(size), &mut io::sink())?;
                }
            }
        }
    }

    /// Element id, marker bits kept (as the spec writes them). `None` at a clean end.
    fn read_id(&mut self) -> io::Result<Option<u32>> {
        let mut first = [0u8];
        if self.r.read(&mut first)? == 0 {
            return Ok(None);
        }
        let len = first[0].leading_zeros() as usize + 1;
        if len > 4 {
            return Err(bad(format!("bad element id byte {:#x}", first[0])));
        }
        let mut id = first[0] as u32;
        for b in self.read_n(len - 1)? {
            id = id << 8 | b as u32;
        }
        Ok(Some(id))
    }

    /// Element size; `None` for "unknown" (all value bits set).
    fn read_size(&mut self) -> io::Result<Option<u64>> {
        let (value, len) = vint(&mut self.r)?;
        Ok((value != (1u64 << (7 * len)) - 1).then_some(value))
    }

    fn read_n(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.r.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn read_vec(&mut self, size: u64) -> io::Result<Vec<u8>> {
        self.read_n(size as usize)
    }
}

/// A variable-length integer with its marker bit removed, and its length.
fn vint(r: &mut impl Read) -> io::Result<(u64, usize)> {
    let mut first = [0u8];
    r.read_exact(&mut first)?;
    let len = first[0].leading_zeros() as usize + 1;
    if len > 8 {
        return Err(bad("bad size byte 0".into()));
    }
    let mut value = (first[0] as u64) & (0xff >> len);
    let mut rest = vec![0u8; len - 1];
    r.read_exact(&mut rest)?;
    for b in rest {
        value = value << 8 | b as u64;
    }
    Ok((value, len))
}

/// The frame inside a (Simple)Block: track number, timecode, flags, data.
fn block(body: Vec<u8>, simple: bool) -> io::Result<Option<Item>> {
    let mut r = body.as_slice();
    vint(&mut r)?; // track number; there is only one
    if r.len() < 3 {
        return Err(bad("block too short".into()));
    }
    let flags = r[2];
    if flags & 0x06 != 0 {
        tracing::warn!("screen: laced Matroska block skipped");
        return Ok(None);
    }
    let start = body.len() - r.len() + 3;
    let data = body[start..].to_vec();
    // Variable frame rate video comes in BlockGroups, which have no key
    // frame flag; an IDR picture is a key frame either way.
    let key = (simple && flags & 0x80 != 0) || has_idr(&data);
    Ok(Some(Item::Frame { key, data }))
}

/// True if a frame (NAL units with 4-byte lengths) holds an IDR slice.
fn has_idr(mut data: &[u8]) -> bool {
    while data.len() > 4 {
        let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        if data[4] & 0x1f == 5 {
            return true;
        }
        data = data.get(4 + len..).unwrap_or_default();
    }
    false
}

fn bad(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(id: u32, body: &[u8]) -> Vec<u8> {
        let mut out = id.to_be_bytes().iter().copied().skip_while(|&b| b == 0).collect::<Vec<_>>();
        // 8-byte size, as matroskamux writes them.
        out.push(0x01);
        out.extend(&(body.len() as u64).to_be_bytes()[1..]);
        out.extend(body);
        out
    }

    fn unknown_size(id: u32) -> Vec<u8> {
        let mut out = id.to_be_bytes().iter().copied().skip_while(|&b| b == 0).collect::<Vec<_>>();
        out.extend([0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        out
    }

    #[test]
    fn reads_codec_setup_and_frames() {
        let mut s = element(0x1A45_DFA3, &[0x42, 0x82, 0x88]);
        s.extend(unknown_size(SEGMENT));
        s.extend(element(0x1549_A966, b"info to skip"));
        let entry = [element(0xD7, &[1]), element(CODEC_PRIVATE, &[1, 0x42, 0xc0, 0x28])].concat();
        s.extend(element(TRACKS, &element(TRACK_ENTRY, &entry)));
        s.extend(unknown_size(CLUSTER));
        s.extend([0xE7, 0x81, 0x00]); // cluster timecode, 1-byte size
        s.extend(element(SIMPLE_BLOCK, &[0x81, 0, 0, 0x80, 0, 0, 0, 1, 0x65]));
        s.extend(element(SIMPLE_BLOCK, &[0x81, 0, 33, 0x00, 0, 0, 0, 1, 0x41]));
        s.extend(unknown_size(CLUSTER));
        s.extend(element(SIMPLE_BLOCK, &[0x81, 0, 0, 0x00, 9]));

        let mut r = Reader::new(s.as_slice());
        assert_eq!(r.next_item().unwrap(), Some(Item::CodecPrivate(vec![1, 0x42, 0xc0, 0x28])));
        assert_eq!(r.next_item().unwrap(), Some(Item::Frame { key: true, data: vec![0, 0, 0, 1, 0x65] }));
        assert_eq!(r.next_item().unwrap(), Some(Item::Frame { key: false, data: vec![0, 0, 0, 1, 0x41] }));
        assert_eq!(r.next_item().unwrap(), Some(Item::Frame { key: false, data: vec![9] }));
        assert_eq!(r.next_item().unwrap(), None);
    }

    #[test]
    fn finds_idr_in_block_group() {
        let mut s = unknown_size(CLUSTER);
        // AUD, then an IDR slice.
        let group = element(BLOCK, &[0x81, 0, 0, 0x00, 0, 0, 0, 2, 0x09, 0x10, 0, 0, 0, 1, 0x65]);
        s.extend(element(BLOCK_GROUP, &group));
        s.extend(element(BLOCK_GROUP, &element(BLOCK, &[0x81, 0, 1, 0x00, 0, 0, 0, 1, 0x41])));
        let mut r = Reader::new(s.as_slice());
        assert!(matches!(r.next_item().unwrap(), Some(Item::Frame { key: true, .. })));
        assert!(matches!(r.next_item().unwrap(), Some(Item::Frame { key: false, .. })));
    }

    #[test]
    fn rejects_unknown_size_leaf() {
        let s = unknown_size(SIMPLE_BLOCK);
        assert!(Reader::new(s.as_slice()).next_item().is_err());
    }
}
