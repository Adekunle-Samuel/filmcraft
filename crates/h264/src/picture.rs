//! Picture buffers and per-macroblock state.

use std::sync::Arc;

/// Planar 4:2:0 8-bit sample buffers (MB-aligned dimensions, stride == width).
#[derive(Clone)]
pub struct Planes {
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
}

impl Planes {
    pub fn new(width: usize, height: usize) -> Self {
        let (cw, ch) = (width / 2, height / 2);
        Planes { y: vec![0; width * height], cb: vec![128; cw * ch], cr: vec![128; cw * ch], width, height, cwidth: cw, cheight: ch }
    }
    pub fn gray(width: usize, height: usize) -> Self {
        let mut p = Self::new(width, height);
        p.y.fill(128);
        p
    }
}

/// Macroblock coding category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MbKind {
    #[default]
    None,
    I4x4,
    I8x8,
    I16x16,
    IPcm,
    PSkip,
    BSkip,
    BDirect16x16,
    /// Any other P or B inter macroblock.
    Inter,
}

impl MbKind {
    #[inline]
    pub fn is_intra(self) -> bool {
        matches!(self, MbKind::I4x4 | MbKind::I8x8 | MbKind::I16x16 | MbKind::IPcm)
    }
}

/// Per-macroblock state kept for the whole picture (neighbour derivations, deblocking, co-located data).
#[derive(Clone, Copy)]
pub struct MbState {
    /// Slice number within the picture; `u32::MAX` = not (yet) decoded.
    pub slice_num: u32,
    pub kind: MbKind,
    pub transform_8x8: bool,
    /// coded_block_pattern: bits 0..3 luma, bits 4..5 chroma.
    pub cbp: u8,
    pub qp: u8,
    /// QPc for Cb / Cr (used by deblocking).
    pub qpc: [u8; 2],
    pub intra_chroma_mode: u8,
    /// Intra4x4PredMode / Intra8x8PredMode per 4x4 block (raster order).
    pub intra_modes: [u8; 16],
    /// CAVLC: TotalCoeff per luma 4x4 block (raster). CABAC: coded_block_flag.
    pub nnz: [u8; 16],
    /// Same for chroma AC blocks [Cb, Cr][raster 2x2].
    pub nnz_c: [[u8; 4]; 2],
    /// CABAC coded_block_flag of DC blocks: bit0 luma (I16x16), bit1 Cb, bit2 Cr.
    pub cbf_dc: u8,
    /// Luma 4x4 blocks (raster bit) with non-zero coefficients, for deblocking bS = 2.
    pub nz_mask: u16,
    pub ref_idx: [[i8; 4]; 2],
    /// Motion vectors per 4x4 block (raster order).
    pub mv: [[[i16; 2]; 16]; 2],
    /// |mvd| per 4x4 block (raster), saturated, for CABAC context selection.
    pub mvd: [[[u8; 2]; 16]; 2],
    /// Bit per 8x8 block predicted in direct mode.
    pub direct8x8: u8,
}

impl MbState {
    #[inline]
    pub fn kind_is_skip(&self) -> bool {
        matches!(self.kind, MbKind::PSkip | MbKind::BSkip)
    }
}

impl Default for MbState {
    fn default() -> Self {
        MbState {
            slice_num: u32::MAX,
            kind: MbKind::None,
            transform_8x8: false,
            cbp: 0,
            qp: 0,
            qpc: [0; 2],
            intra_chroma_mode: 0,
            intra_modes: [2; 16],
            nnz: [0; 16],
            nnz_c: [[0; 4]; 2],
            cbf_dc: 0,
            nz_mask: 0,
            ref_idx: [[-1; 4]; 2],
            mv: [[[0; 2]; 16]; 2],
            mvd: [[[0; 2]; 16]; 2],
            direct8x8: 0,
        }
    }
}

/// Motion data of a decoded picture, used as co-located data for direct prediction.
#[derive(Clone, Default)]
pub struct MotionField {
    /// Per 4x4 block: index mb * 16 + raster.
    pub mv: [Vec<[i16; 2]>; 2],
    /// Per 8x8 block: index mb * 4 + b8.
    pub ref_idx: [Vec<i8>; 2],
    /// Unique id of the referenced picture per 8x8 block (u32::MAX = none).
    pub ref_id: [Vec<u32>; 2],
    pub intra: Vec<bool>,
}

/// A decoded frame as stored in the DPB and referenced by later pictures.
pub struct Frame {
    pub id: u32,
    pub poc: i32,
    pub planes: Planes,
    pub motion: MotionField,
}

pub type FrameRef = Arc<Frame>;

/// An entry of RefPicList0/1 for the current slice.
#[derive(Clone)]
pub struct RefPic {
    pub frame: FrameRef,
    pub long_term: bool,
}

impl RefPic {
    pub fn poc(&self) -> i32 {
        self.frame.poc
    }
    pub fn id(&self) -> u32 {
        self.frame.id
    }
}
