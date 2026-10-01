//! TOC byte and frame packing (RFC 6716 §3).

use crate::Error;

/// Coding mode of an Opus frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    SilkOnly,
    Hybrid,
    CeltOnly,
}

/// Audio bandwidth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bandwidth {
    /// 4 kHz.
    Narrowband,
    /// 6 kHz.
    Mediumband,
    /// 8 kHz.
    Wideband,
    /// 12 kHz.
    SuperWideband,
    /// 20 kHz.
    Fullband,
}

impl Bandwidth {
    /// Number of CELT bands coded for this bandwidth.
    pub fn celt_end_band(self) -> usize {
        match self {
            Bandwidth::Narrowband => 13,
            Bandwidth::Mediumband | Bandwidth::Wideband => 17,
            Bandwidth::SuperWideband => 19,
            Bandwidth::Fullband => 21,
        }
    }
}

/// Decoded table-of-contents byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Toc {
    pub config: u8,
    pub mode: Mode,
    pub bandwidth: Bandwidth,
    pub stereo: bool,
    /// Frame count code (0..=3).
    pub code: u8,
}

impl Toc {
    pub fn parse(b: u8) -> Toc {
        let config = b >> 3;
        let (mode, bandwidth) = match config {
            0..=3 => (Mode::SilkOnly, Bandwidth::Narrowband),
            4..=7 => (Mode::SilkOnly, Bandwidth::Mediumband),
            8..=11 => (Mode::SilkOnly, Bandwidth::Wideband),
            12..=13 => (Mode::Hybrid, Bandwidth::SuperWideband),
            14..=15 => (Mode::Hybrid, Bandwidth::Fullband),
            16..=19 => (Mode::CeltOnly, Bandwidth::Narrowband),
            20..=23 => (Mode::CeltOnly, Bandwidth::Wideband),
            24..=27 => (Mode::CeltOnly, Bandwidth::SuperWideband),
            _ => (Mode::CeltOnly, Bandwidth::Fullband),
        };
        Toc { config, mode, bandwidth, stereo: b & 4 != 0, code: b & 3 }
    }

    /// Samples per frame at 48 kHz.
    pub fn frame_samples_48k(&self) -> usize {
        self.frame_samples(48000)
    }

    /// Samples per frame at `rate`.
    pub fn frame_samples(&self, rate: u32) -> usize {
        let rate = rate as usize;
        let c = self.config as usize;
        match self.mode {
            Mode::CeltOnly => (rate << (c & 3)) / 400,
            Mode::Hybrid => {
                if c & 1 != 0 {
                    rate / 50
                } else {
                    rate / 100
                }
            }
            Mode::SilkOnly => match c & 3 {
                3 => rate * 60 / 1000,
                s => (rate << s) / 100,
            },
        }
    }

    pub fn channels(&self) -> usize {
        if self.stereo { 2 } else { 1 }
    }
}

/// A parsed packet: TOC plus the byte ranges of each frame.
#[derive(Clone, Debug)]
pub struct Packet<'a> {
    pub toc: Toc,
    pub frames: Vec<&'a [u8]>,
    /// Bytes of padding (code 3 only).
    pub padding: usize,
}

fn parse_size(data: &[u8]) -> Option<(usize, usize)> {
    match data.first() {
        None => None,
        Some(&b) if b < 252 => Some((b as usize, 1)),
        Some(&b) => data.get(1).map(|&b1| (4 * b1 as usize + b as usize, 2)),
    }
}

impl<'a> Packet<'a> {
    /// Parses a packet (RFC 6716 §3.2), enforcing requirements R1–R7.
    pub fn parse(data: &'a [u8]) -> Result<Packet<'a>, Error> {
        const BAD: Error = Error::InvalidPacket("malformed packet framing");
        if data.is_empty() {
            return Err(Error::InvalidPacket("empty packet"));
        }
        let toc = Toc::parse(data[0]);
        let fs = toc.frame_samples_48k();
        let mut p = &data[1..];
        let mut frames = Vec::new();
        let mut padding = 0usize;
        match toc.code {
            0 => frames.push(p),
            1 => {
                if !p.len().is_multiple_of(2) {
                    return Err(BAD);
                }
                let h = p.len() / 2;
                frames.push(&p[..h]);
                frames.push(&p[h..]);
            }
            2 => {
                let (s, n) = parse_size(p).ok_or(BAD)?;
                p = &p[n..];
                if s > p.len() {
                    return Err(BAD);
                }
                frames.push(&p[..s]);
                frames.push(&p[s..]);
            }
            _ => {
                let &ch = p.first().ok_or(BAD)?;
                p = &p[1..];
                let count = (ch & 0x3F) as usize;
                if count == 0 || fs * count > 5760 {
                    return Err(BAD);
                }
                let mut len = p.len() as isize;
                if ch & 0x40 != 0 {
                    loop {
                        if len <= 0 {
                            return Err(BAD);
                        }
                        let b = p[0];
                        p = &p[1..];
                        len -= 1;
                        let tmp = if b == 255 { 254 } else { b as isize };
                        len -= tmp;
                        padding += tmp as usize;
                        if b != 255 {
                            break;
                        }
                    }
                }
                if len < 0 {
                    return Err(BAD);
                }
                let len = len as usize;
                // `p` still includes the padding at its end; only the first `len` bytes are frames.
                let body = &p[..len.min(p.len())];
                if ch & 0x80 != 0 {
                    // VBR
                    let mut q = body;
                    let mut sizes = Vec::with_capacity(count);
                    let mut remaining = len as isize;
                    for _ in 0..count - 1 {
                        let (s, n) = parse_size(q).ok_or(BAD)?;
                        q = &q[n..];
                        remaining -= n as isize;
                        if s as isize > remaining {
                            return Err(BAD);
                        }
                        remaining -= s as isize;
                        sizes.push(s);
                    }
                    if remaining < 0 {
                        return Err(BAD);
                    }
                    sizes.push(remaining as usize);
                    let mut off = 0;
                    for s in sizes {
                        frames.push(&q[off..off + s]);
                        off += s;
                    }
                } else {
                    let s = len / count;
                    if s * count != len {
                        return Err(BAD);
                    }
                    for i in 0..count {
                        frames.push(&body[i * s..(i + 1) * s]);
                    }
                }
            }
        }
        if frames.last().is_some_and(|f| f.len() > 1275) || frames.iter().any(|f| f.len() > 1275) {
            return Err(BAD);
        }
        Ok(Packet { toc, frames, padding })
    }
}

/// Number of samples (at 48 kHz) in a packet, or an error for malformed framing.
pub fn packet_samples_48k(data: &[u8]) -> Result<usize, Error> {
    let p = Packet::parse(data)?;
    Ok(p.toc.frame_samples_48k() * p.frames.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_configs() {
        for cfg in 0u8..32 {
            let t = Toc::parse(cfg << 3);
            let ms10 = t.frame_samples(48000) * 10 / 48; // tenths of ms
            let expect = match cfg {
                0..=11 => [100, 200, 400, 600][cfg as usize & 3],
                12..=15 => [100, 200][cfg as usize & 1],
                _ => [25, 50, 100, 200][cfg as usize & 3],
            };
            assert_eq!(ms10, expect, "config {cfg}");
        }
        assert!(Toc::parse(0x04).stereo);
    }

    #[test]
    fn frame_codes() {
        // code 0
        let p = Packet::parse(&[0x00, 1, 2, 3]).unwrap();
        assert_eq!(p.frames, vec![&[1u8, 2, 3][..]]);
        // code 1 (odd payload is invalid)
        assert!(Packet::parse(&[0x01, 1, 2, 3]).is_err());
        let p = Packet::parse(&[0x01, 1, 2, 3, 4]).unwrap();
        assert_eq!(p.frames.len(), 2);
        // code 2
        let p = Packet::parse(&[0x02, 1, 9, 8, 7]).unwrap();
        assert_eq!(p.frames, vec![&[9u8][..], &[8u8, 7][..]]);
        assert!(Packet::parse(&[0x02, 5, 1]).is_err());
        // code 3 CBR with padding: 2 frames of 2 bytes, 3 bytes padding
        let p = Packet::parse(&[0x03, 0x42, 3, 1, 2, 3, 4, 0, 0, 0]).unwrap();
        assert_eq!(p.frames, vec![&[1u8, 2][..], &[3u8, 4][..]]);
        assert_eq!(p.padding, 3);
        // code 3 VBR
        let p = Packet::parse(&[0x03, 0x83, 1, 2, 10, 20, 21, 30]).unwrap();
        assert_eq!(p.frames, vec![&[10u8][..], &[20u8, 21][..], &[30u8][..]]);
        // too many frames (> 120 ms)
        assert!(Packet::parse(&[0x03 | (3 << 3), 0x03, 0]).is_err());
        // zero frames
        assert!(Packet::parse(&[0x03, 0x00]).is_err());
        assert!(Packet::parse(&[]).is_err());
    }

    #[test]
    fn two_byte_sizes() {
        let mut pkt = vec![0x02u8, 252, 1];
        pkt.extend(std::iter::repeat_n(7u8, 256));
        pkt.extend([1, 2]);
        let p = Packet::parse(&pkt).unwrap();
        assert_eq!(p.frames[0].len(), 256);
        assert_eq!(p.frames[1], &[1, 2]);
    }
}
