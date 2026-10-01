//! Symbol (arithmetic) decoder, spec 8.2.
//!
//! The state follows the specification literally: `SymbolValue` / `SymbolRange` are 15/16-bit
//! quantities renormalised by reading `bits` new bits at a time, and `SymbolMaxBits` counts the
//! bits still available (going negative while zero padding is consumed).

const EC_PROB_SHIFT: u32 = 6;
const EC_MIN_PROB: u32 = 4;

pub(crate) struct SymbolDecoder<'a> {
    data: &'a [u8],
    /// Bit position of the next bit to read.
    pos: usize,
    end: usize,
    value: u32,
    range: u32,
    max_bits: i64,
    pub disable_update: bool,
}

impl<'a> SymbolDecoder<'a> {
    /// `init_symbol( sz )` over `data` (the tile's bytes).
    pub fn new(data: &'a [u8], disable_update: bool) -> Self {
        let sz = data.len();
        let mut d = SymbolDecoder { data, pos: 0, end: sz * 8, value: 0, range: 1 << 15, max_bits: 8 * sz as i64 - 15, disable_update };
        let num_bits = (sz * 8).min(15) as u32;
        let buf = d.read_bits(num_bits);
        let padded = buf << (15 - num_bits);
        d.value = ((1 << 15) - 1) ^ padded;
        d
    }

    #[inline(always)]
    fn read_bits(&mut self, n: u32) -> u32 {
        let mut x = 0u32;
        for _ in 0..n {
            let bit = if self.pos < self.end { (self.data[self.pos >> 3] >> (7 - (self.pos & 7))) & 1 } else { 0 };
            self.pos += 1;
            x = (x << 1) | bit as u32;
        }
        x
    }

    /// `read_symbol( cdf )`: `cdf` has N + 1 entries (the last is the adaptation counter).
    #[inline]
    pub fn read_symbol(&mut self, cdf: &mut [u16]) -> usize {
        let n = cdf.len() - 1;
        let symbol = self.decode(cdf, n);
        if !self.disable_update {
            update_cdf(cdf, n, symbol);
        }
        symbol
    }

    #[inline(always)]
    fn decode(&mut self, cdf: &[u16], n: usize) -> usize {
        let mut cur = self.range;
        let mut symbol = 0usize;
        let mut prev;
        loop {
            prev = cur;
            let f = (1u32 << 15) - cdf[symbol] as u32;
            cur = ((self.range >> 8) * (f >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT);
            cur += EC_MIN_PROB * (n - symbol - 1) as u32;
            if self.value >= cur {
                break;
            }
            symbol += 1;
        }
        self.range = prev - cur;
        self.value -= cur;
        self.renormalize();
        symbol
    }

    #[inline(always)]
    fn renormalize(&mut self) {
        let bits = 15 - (31 - self.range.leading_zeros());
        if bits == 0 {
            return;
        }
        self.range <<= bits;
        let num_bits = (bits as i64).min(self.max_bits.max(0)) as u32;
        let new_data = self.read_bits(num_bits);
        let padded = new_data << (bits - num_bits);
        self.value = padded ^ (((self.value + 1) << bits) - 1);
        self.max_bits -= bits as i64;
    }

    /// `read_bool()`
    #[inline]
    pub fn read_bool(&mut self) -> u32 {
        let cdf = [1u16 << 14, 1 << 15, 0];
        self.decode(&cdf, 2) as u32
    }

    /// `read_literal( n )` (L(n))
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut x = 0;
        for _ in 0..n {
            x = (x << 1) | self.read_bool();
        }
        x
    }

    /// NS(n)
    pub fn read_ns(&mut self, n: u32) -> u32 {
        if n <= 1 {
            return 0;
        }
        let w = 32 - n.leading_zeros();
        let m = (1u32 << w) - n;
        let v = self.read_literal(w - 1);
        if v < m {
            return v;
        }
        let extra = self.read_literal(1);
        (v << 1) - m + extra
    }

    /// SymbolMaxBits (for the exit process checks).
    pub fn max_bits(&self) -> i64 {
        self.max_bits
    }
}

/// CDF adaptation (8.2.6).
#[inline(always)]
pub(crate) fn update_cdf(cdf: &mut [u16], n: usize, symbol: usize) {
    let count = cdf[n];
    let rate = 3 + (count > 15) as u32 + (count > 31) as u32 + (31 - (n as u32).leading_zeros()).min(2);
    let mut tmp = 0u32;
    for i in 0..n - 1 {
        if i == symbol {
            tmp = 1 << 15;
        }
        let c = cdf[i] as u32;
        if tmp < c {
            cdf[i] = (c - ((c - tmp) >> rate)) as u16;
        } else {
            cdf[i] = (c + ((tmp - c) >> rate)) as u16;
        }
    }
    cdf[n] += (count < 32) as u16;
}
