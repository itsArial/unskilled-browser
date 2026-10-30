//! DEFLATE decoder (RFC 1951): stored, fixed-Huffman and dynamic-Huffman blocks. Canonical
//! Huffman codes are decoded bit by bit against per-length code counts, which is small and fast
//! enough for extension archives.

use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The input ended in the middle of a block.
    Truncated,
    Invalid(&'static str),
    /// Output would exceed the caller's limit (zip bombs).
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "compressed data is truncated"),
            Self::Invalid(why) => write!(f, "invalid compressed data: {why}"),
            Self::TooLarge => write!(f, "compressed data expands too far"),
        }
    }
}

const MAX_BITS: usize = 15;

const LENGTH_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
/// Order the code-length code lengths are stored in.
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// LSB-first bit reader.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, buf: 0, count: 0 }
    }

    fn bits(&mut self, n: u32) -> Result<u32, Error> {
        while self.count < n {
            let byte = *self.data.get(self.pos).ok_or(Error::Truncated)?;
            self.pos += 1;
            self.buf |= (byte as u64) << self.count;
            self.count += 8;
        }
        let value = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.count -= n;
        Ok(value)
    }

    /// Drops the bits left in the current byte (stored blocks start byte-aligned).
    fn align(&mut self) {
        let drop = self.count % 8;
        self.buf >>= drop;
        self.count -= drop;
    }
}

/// A canonical Huffman code: how many codes have each length, and the symbols in code order.
struct Huffman {
    counts: [u16; MAX_BITS + 1],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, Error> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &len in lengths {
            counts[len as usize] += 1;
        }
        counts[0] = 0;
        // Over-subscribed codes can't be decoded unambiguously.
        let mut left: i32 = 1;
        for &count in &counts[1..] {
            left = (left << 1) - count as i32;
            if left < 0 {
                return Err(Error::Invalid("over-subscribed code"));
            }
        }
        let mut offsets = [0u16; MAX_BITS + 2];
        for len in 1..=MAX_BITS {
            offsets[len + 1] = offsets[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbols[offsets[len as usize] as usize] = symbol as u16;
                offsets[len as usize] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Result<u16, Error> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=MAX_BITS {
            code |= bits.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return Ok(self.symbols[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::Invalid("bad code"))
    }
}

/// Decompresses a raw DEFLATE stream, producing at most `limit` bytes.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut bits = Bits::new(data);
    let mut out = Vec::new();
    loop {
        let last = bits.bits(1)? == 1;
        match bits.bits(2)? {
            0 => stored(&mut bits, &mut out, limit)?,
            1 => {
                let (lit, dist) = fixed();
                codes(&mut bits, &mut out, &lit, &dist, limit)?;
            }
            2 => {
                let (lit, dist) = dynamic(&mut bits)?;
                codes(&mut bits, &mut out, &lit, &dist, limit)?;
            }
            _ => return Err(Error::Invalid("reserved block type")),
        }
        if last {
            return Ok(out);
        }
    }
}

fn stored(bits: &mut Bits, out: &mut Vec<u8>, limit: usize) -> Result<(), Error> {
    bits.align();
    let len = bits.bits(16)?;
    let nlen = bits.bits(16)?;
    if len != !nlen & 0xFFFF {
        return Err(Error::Invalid("stored block length"));
    }
    // After alignment the buffer holds whole bytes only; hand them out first.
    for _ in 0..len {
        if out.len() >= limit {
            return Err(Error::TooLarge);
        }
        out.push(bits.bits(8)? as u8);
    }
    Ok(())
}

fn fixed() -> (Huffman, Huffman) {
    let mut lengths = [0u8; 288];
    lengths[..144].fill(8);
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths[280..].fill(8);
    let lit = Huffman::new(&lengths).expect("fixed literal code");
    let dist = Huffman::new(&[5u8; 30]).expect("fixed distance code");
    (lit, dist)
}

fn dynamic(bits: &mut Bits) -> Result<(Huffman, Huffman), Error> {
    let nlen = bits.bits(5)? as usize + 257;
    let ndist = bits.bits(5)? as usize + 1;
    let ncode = bits.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(Error::Invalid("too many codes"));
    }
    let mut cl_lengths = [0u8; 19];
    for &i in &CL_ORDER[..ncode] {
        cl_lengths[i] = bits.bits(3)? as u8;
    }
    let cl = Huffman::new(&cl_lengths)?;

    let mut lengths = vec![0u8; nlen + ndist];
    let mut i = 0;
    while i < nlen + ndist {
        let symbol = cl.decode(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => {
                let previous = *lengths.get(i.wrapping_sub(1)).filter(|_| i > 0).ok_or(Error::Invalid("repeat with no previous length"))?;
                (previous, 3 + bits.bits(2)? as usize)
            }
            17 => (0, 3 + bits.bits(3)? as usize),
            _ => (0, 11 + bits.bits(7)? as usize),
        };
        if i + repeat > lengths.len() {
            return Err(Error::Invalid("too many lengths"));
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    if lengths[256] == 0 {
        return Err(Error::Invalid("no end-of-block code"));
    }
    Ok((Huffman::new(&lengths[..nlen])?, Huffman::new(&lengths[nlen..])?))
}

fn codes(bits: &mut Bits, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman, limit: usize) -> Result<(), Error> {
    loop {
        let symbol = lit.decode(bits)? as usize;
        match symbol {
            0..=255 => {
                if out.len() >= limit {
                    return Err(Error::TooLarge);
                }
                out.push(symbol as u8);
            }
            256 => return Ok(()),
            _ => {
                let i = symbol - 257;
                if i >= LENGTH_BASE.len() {
                    return Err(Error::Invalid("bad length symbol"));
                }
                let len = LENGTH_BASE[i] as usize + bits.bits(LENGTH_EXTRA[i] as u32)? as usize;
                let d = dist.decode(bits)? as usize;
                if d >= DIST_BASE.len() {
                    return Err(Error::Invalid("bad distance symbol"));
                }
                let distance = DIST_BASE[d] as usize + bits.bits(DIST_EXTRA[d] as u32)? as usize;
                if distance > out.len() {
                    return Err(Error::Invalid("distance before start"));
                }
                if out.len() + len > limit {
                    return Err(Error::TooLarge);
                }
                // Byte by byte: the source may overlap what's being written (runs).
                let start = out.len() - distance;
                for k in 0..len {
                    out.push(out[start + k]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_block() {
        // BFINAL=1, BTYPE=00, LEN=5, NLEN=!5, "hello".
        let data = [0x01, 0x05, 0x00, 0xFA, 0xFF, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(inflate(&data, 100).unwrap(), b"hello");
    }

    #[test]
    fn fixed_huffman() {
        // `printf 'hello hello hello' | gzip -1`, raw deflate part.
        let data = [0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x90, 0x00];
        assert_eq!(inflate(&data, 100).unwrap(), b"hello hello hello");
    }

    #[test]
    fn dynamic_huffman() {
        let out = inflate(include_bytes!("../tests/fixtures/dynamic.deflate"), 1 << 20).unwrap();
        assert_eq!(out, include_bytes!("../tests/fixtures/dynamic.txt"));
    }

    #[test]
    fn limit_stops_bombs() {
        let data = [0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x90, 0x00];
        assert_eq!(inflate(&data, 8), Err(Error::TooLarge));
    }
}
