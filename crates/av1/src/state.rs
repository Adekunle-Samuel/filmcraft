//! Per-frame decoding state shared by the tiles and the post-filters.

use crate::frame::{FrameBuf, MiInfo};
use crate::header::{FrameHeader, SequenceHeader};
use crate::spec_tables::*;

pub(crate) struct FrameState {
    pub seq: SequenceHeader,
    pub fh: FrameHeader,
    pub num_planes: usize,
    pub ssx: usize,
    pub ssy: usize,
    pub bit_depth: u32,
    pub cur: FrameBuf,
    pub mi: MiInfo,
    pub above_level: [Vec<u8>; 3],
    pub above_dc: [Vec<u8>; 3],
    pub above_seg_pred: Vec<u8>,
    pub left_level: [Vec<u8>; 3],
    pub left_dc: [Vec<u8>; 3],
    pub left_seg_pred: Vec<u8>,
    /// cdef_idx per 64x64 block.
    cdef: Vec<i8>,
    cdef_cols: usize,
    /// LoopfilterTxSizes[ plane ][ row ][ col ] in 4x4 units of each plane.
    pub lf_tx_size: [Vec<u8>; 3],
    pub lr_unit_rows: [usize; 3],
    pub lr_unit_cols: [usize; 3],
    pub lr_type: [Vec<u8>; 3],
    pub lr_wiener: [Vec<[[i8; 3]; 2]>; 3],
    pub lr_sgr_set: [Vec<u8>; 3],
    pub lr_sgr_xqd: [Vec<[i8; 2]>; 3],
    /// TileIntraFrameYModeCdf (reset per tile).
    pub intra_frame_y_mode_cdf: [[[u16; 14]; 5]; 5],
}

fn count_units_in_frame(unit_size: usize, frame_size: usize) -> usize {
    ((frame_size + (unit_size >> 1)) / unit_size).max(1)
}

impl FrameState {
    pub fn new(seq: &SequenceHeader, fh: &FrameHeader) -> FrameState {
        let c = &seq.color;
        let (ssx, ssy) = (c.subsampling_x as usize, c.subsampling_y as usize);
        let mi_cols = fh.mi_cols as usize;
        let mi_rows = fh.mi_rows as usize;
        let cur = FrameBuf::new(fh.frame_width as usize, fh.frame_height as usize, c.num_planes, ssx, ssy, c.bit_depth);
        let cdef_cols = mi_cols.div_ceil(16) + 2;
        let cdef_rows = mi_rows.div_ceil(16) + 2;
        let mut lr_unit_rows = [0; 3];
        let mut lr_unit_cols = [0; 3];
        let mut lr_type: [Vec<u8>; 3] = Default::default();
        let mut lr_wiener: [Vec<[[i8; 3]; 2]>; 3] = Default::default();
        let mut lr_sgr_set: [Vec<u8>; 3] = Default::default();
        let mut lr_sgr_xqd: [Vec<[i8; 2]>; 3] = Default::default();
        for plane in 0..c.num_planes {
            if fh.lr.frame_restoration_type[plane] == RESTORE_NONE as u8 {
                continue;
            }
            let sub_x = if plane == 0 { 0 } else { ssx };
            let sub_y = if plane == 0 { 0 } else { ssy };
            let unit = fh.lr.loop_restoration_size[plane] as usize;
            lr_unit_rows[plane] = count_units_in_frame(unit, (fh.frame_height as usize + sub_y) >> sub_y);
            lr_unit_cols[plane] = count_units_in_frame(unit, (fh.upscaled_width as usize + sub_x) >> sub_x);
            let n = lr_unit_rows[plane] * lr_unit_cols[plane];
            lr_type[plane] = vec![RESTORE_NONE as u8; n];
            lr_wiener[plane] = vec![[[0; 3]; 2]; n];
            lr_sgr_set[plane] = vec![0; n];
            lr_sgr_xqd[plane] = vec![[0; 2]; n];
        }
        FrameState {
            seq: seq.clone(),
            fh: fh.clone(),
            num_planes: c.num_planes,
            ssx,
            ssy,
            bit_depth: c.bit_depth as u32,
            cur,
            mi: MiInfo::new(mi_cols, mi_rows),
            above_level: std::array::from_fn(|_| vec![0; mi_cols + 64]),
            above_dc: std::array::from_fn(|_| vec![0; mi_cols + 64]),
            above_seg_pred: vec![0; mi_cols + 64],
            left_level: std::array::from_fn(|_| vec![0; mi_rows + 64]),
            left_dc: std::array::from_fn(|_| vec![0; mi_rows + 64]),
            left_seg_pred: vec![0; mi_rows + 64],
            cdef: vec![-1; cdef_cols * cdef_rows],
            cdef_cols,
            lf_tx_size: std::array::from_fn(|_| vec![0; (mi_cols + 32) * (mi_rows + 32)]),
            lr_unit_rows,
            lr_unit_cols,
            lr_type,
            lr_wiener,
            lr_sgr_set,
            lr_sgr_xqd,
            intra_frame_y_mode_cdf: DEFAULT_INTRA_FRAME_Y_MODE_CDF,
        }
    }

    pub fn plane_residual_size(&self, subsize: usize, plane: usize) -> usize {
        let sx = if plane > 0 { self.ssx } else { 0 };
        let sy = if plane > 0 { self.ssy } else { 0 };
        SUBSAMPLED_SIZE[subsize][sx][sy] as usize
    }

    #[inline]
    pub fn cdef_idx(&self, r: usize, c: usize) -> i8 {
        self.cdef[(r >> 4) * self.cdef_cols + (c >> 4)]
    }

    #[inline]
    pub fn set_cdef_idx(&mut self, r: usize, c: usize, v: i8) {
        let i = (r >> 4) * self.cdef_cols + (c >> 4);
        if i < self.cdef.len() {
            self.cdef[i] = v;
        }
    }

    #[inline]
    pub fn lf_tx_size(&self, plane: usize, row: usize, col: usize) -> usize {
        self.lf_tx_size[plane][row * (self.mi.cols + 32) + col] as usize
    }

    #[inline]
    pub fn set_lf_tx_size(&mut self, plane: usize, row: usize, col: usize, v: u8) {
        let stride = self.mi.cols + 32;
        if col < stride && row < self.mi.rows + 32 {
            self.lf_tx_size[plane][row * stride + col] = v;
        }
    }
}
