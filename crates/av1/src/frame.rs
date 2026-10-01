//! Frame sample buffers and per-4x4 mode info storage.

/// One plane of samples (u16 at every bit depth).
#[derive(Clone, Default)]
pub struct Plane {
    pub data: Vec<u16>,
    pub stride: usize,
    /// Allocated rows.
    pub rows: usize,
}

impl Plane {
    pub fn new(width: usize, height: usize) -> Plane {
        Plane { data: vec![0; width * height], stride: width, rows: height }
    }
    #[inline(always)]
    pub fn at(&self, x: usize, y: usize) -> u16 {
        self.data[y * self.stride + x]
    }
    #[inline(always)]
    pub fn set(&mut self, x: usize, y: usize, v: u16) {
        self.data[y * self.stride + x] = v;
    }
    pub fn row(&self, y: usize) -> &[u16] {
        &self.data[y * self.stride..(y + 1) * self.stride]
    }
    pub fn row_mut(&mut self, y: usize) -> &mut [u16] {
        &mut self.data[y * self.stride..(y + 1) * self.stride]
    }
}

/// A frame's three planes, sized to the macroblock-aligned coded area plus margins.
#[derive(Clone, Default)]
pub struct FrameBuf {
    pub planes: [Plane; 3],
    pub num_planes: usize,
    pub subsampling_x: usize,
    pub subsampling_y: usize,
    pub bit_depth: u8,
    /// Frame dimensions (luma) the samples are valid for.
    pub width: usize,
    pub height: usize,
}

impl FrameBuf {
    /// Allocate a frame for a coded area of `width` x `height` luma samples (rounded up to the
    /// 128x128 superblock grid plus 160 samples of slack for blocks extending past the edge).
    pub fn new(width: usize, height: usize, num_planes: usize, subsampling_x: usize, subsampling_y: usize, bit_depth: u8) -> FrameBuf {
        let aw = width.div_ceil(128) * 128 + 160;
        let ah = height.div_ceil(128) * 128 + 160;
        let mut planes: [Plane; 3] = Default::default();
        planes[0] = Plane::new(aw, ah);
        for p in planes.iter_mut().take(num_planes).skip(1) {
            *p = Plane::new(aw >> subsampling_x, ah >> subsampling_y);
        }
        FrameBuf { planes, num_planes, subsampling_x, subsampling_y, bit_depth, width, height }
    }

    pub fn plane_width(&self, plane: usize) -> usize {
        if plane == 0 { self.width } else { (self.width + self.subsampling_x) >> self.subsampling_x }
    }

    pub fn plane_height(&self, plane: usize) -> usize {
        if plane == 0 { self.height } else { (self.height + self.subsampling_y) >> self.subsampling_y }
    }
}

/// A motion vector (row, col) in 1/8 sample units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mv {
    pub row: i16,
    pub col: i16,
}

impl Mv {
    pub const ZERO: Mv = Mv { row: 0, col: 0 };
    pub fn new(row: i32, col: i32) -> Mv {
        Mv { row: row as i16, col: col as i16 }
    }
}

/// Mode info stored for every 4x4 luma position (the arrays the spec indexes by
/// `[ row ][ col ]` in mode-info units).
#[derive(Clone, Default)]
pub struct MiInfo {
    pub cols: usize,
    pub rows: usize,
    pub y_mode: Vec<u8>,
    pub uv_mode: Vec<u8>,
    pub ref_frame: Vec<[i8; 2]>,
    pub mv: Vec<[Mv; 2]>,
    pub is_inter: Vec<bool>,
    pub skip_mode: Vec<bool>,
    pub skip: Vec<bool>,
    pub tx_size: Vec<u8>,
    pub inter_tx_size: Vec<u8>,
    pub mi_size: Vec<u8>,
    pub segment_id: Vec<u8>,
    pub palette_size: [Vec<u8>; 2],
    pub palette_colors: [Vec<[u16; 8]>; 2],
    pub delta_lf: Vec<[i8; 4]>,
    pub comp_group_idx: Vec<u8>,
    pub compound_idx: Vec<u8>,
    pub interp_filter: Vec<[u8; 2]>,
    pub motion_mode: Vec<u8>,
    /// TxTypes[ row ][ col ] (luma 4x4 units).
    pub tx_type: Vec<u8>,
}

impl MiInfo {
    pub fn new(cols: usize, rows: usize) -> MiInfo {
        let n = cols * rows;
        MiInfo {
            cols,
            rows,
            y_mode: vec![0; n],
            uv_mode: vec![0; n],
            ref_frame: vec![[0, -1]; n],
            mv: vec![[Mv::ZERO; 2]; n],
            is_inter: vec![false; n],
            skip_mode: vec![false; n],
            skip: vec![false; n],
            tx_size: vec![0; n],
            inter_tx_size: vec![0; n],
            mi_size: vec![0; n],
            segment_id: vec![0; n],
            palette_size: [vec![0; n], vec![0; n]],
            palette_colors: [vec![[0; 8]; n], vec![[0; 8]; n]],
            delta_lf: vec![[0; 4]; n],
            comp_group_idx: vec![0; n],
            compound_idx: vec![0; n],
            interp_filter: vec![[0; 2]; n],
            motion_mode: vec![0; n],
            tx_type: vec![0; n],
        }
    }

    #[inline(always)]
    pub fn idx(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }
}
