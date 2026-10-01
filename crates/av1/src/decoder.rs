//! Top-level decoder: OBU parsing (5.3), frame / tile group handling, reference frame update
//! (7.20), show-existing-frame and output (7.18).

use std::sync::Arc;

use crate::bits::{BitReader, leb128};
use crate::cdf::Cdfs;
use crate::frame::FrameBuf;
use crate::header::{FrameHeader, HeaderState, LoopFilterDeltas, RefInfo, SegmentationFeatures, SequenceHeader, default_gm_params};
use crate::spec_tables::*;
use crate::state::FrameState;
use crate::tile::TileDecoder;
use crate::{Error, Picture, Result};

const OBU_SEQUENCE_HEADER_T: u32 = 1;
const OBU_TEMPORAL_DELIMITER_T: u32 = 2;
const OBU_FRAME_HEADER_T: u32 = 3;
const OBU_TILE_GROUP_T: u32 = 4;
const OBU_METADATA_T: u32 = 5;
const OBU_FRAME_T: u32 = 6;
const OBU_REDUNDANT_FRAME_HEADER_T: u32 = 7;
const OBU_TILE_LIST_T: u32 = 8;

/// A stored reference frame (FrameStore plus the saved per-frame state).
pub(crate) struct RefFrame {
    pub buf: FrameBuf,
    pub cdfs: Box<Cdfs>,
    pub bit_depth: u8,
    pub subsampling_x: u8,
    pub subsampling_y: u8,
    pub segment_ids: Vec<u8>,
    pub mi_cols: usize,
    pub mi_rows: usize,
    pub film_grain_present: bool,
    pub color: (u8, u8, u8, bool),
    /// SavedRefFrames / SavedMvs (MfRefFrames / MfMvs of 7.19), mi units.
    pub saved_ref_frames: Vec<i8>,
    pub saved_mvs: Vec<[i32; 2]>,
}

/// An AV1 decoder. Feed it temporal units (one MP4 / Matroska sample each, or any run of
/// OBUs in the low-overhead format) and collect the shown frames.
pub struct Decoder {
    seq: Option<SequenceHeader>,
    refs: [RefInfo; NUM_REF_FRAMES],
    ref_frames: [Option<Arc<RefFrame>>; NUM_REF_FRAMES],
    lf_deltas: LoopFilterDeltas,
    seg_features: SegmentationFeatures,
    prev_gm_params: [[i32; 6]; 8],
    current_frame_id: u32,
    /// Frame being decoded (between its header and its last tile group).
    cur: Option<Box<CurFrame>>,
    seen_frame_header: bool,
    /// Apply film grain synthesis to output frames (default true).
    pub apply_film_grain: bool,
}

struct CurFrame {
    fs: FrameState,
    cdfs: Box<Cdfs>,
    saved_cdfs: Option<Box<Cdfs>>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            seq: None,
            refs: Default::default(),
            ref_frames: Default::default(),
            lf_deltas: LoopFilterDeltas::defaults(),
            seg_features: SegmentationFeatures::default(),
            prev_gm_params: default_gm_params(),
            current_frame_id: 0,
            cur: None,
            seen_frame_header: false,
            apply_film_grain: true,
        }
    }

    /// The active sequence header, once one has been decoded.
    pub fn sequence_header(&self) -> Option<&SequenceHeader> {
        self.seq.as_ref()
    }

    /// Decode a chunk of OBUs; returns the frames shown by it.
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<Picture>> {
        // (picture, temporal unit, spatial_id)
        let mut out: Vec<(Picture, usize, u32)> = Vec::new();
        let mut tu = 0usize;
        let mut pos = 0;
        while pos < data.len() {
            let h = data[pos];
            if h & 0x80 != 0 {
                return Err(Error::Invalid("obu_forbidden_bit"));
            }
            let obu_type = ((h >> 3) & 0xf) as u32;
            let ext = h & 4 != 0;
            let has_size = h & 2 != 0;
            let mut p = pos + 1;
            let (mut temporal_id, mut spatial_id) = (0, 0);
            if ext {
                let e = *data.get(p).ok_or(Error::Truncated)?;
                temporal_id = (e >> 5) as u32;
                spatial_id = ((e >> 3) & 3) as u32;
                p += 1;
            }
            let size = if has_size {
                let (v, n) = leb128(&data[p..])?;
                p += n;
                v as usize
            } else {
                data.len() - p
            };
            let end = p.checked_add(size).filter(|&e| e <= data.len()).ok_or(Error::Truncated)?;
            let payload = &data[p..end];
            pos = end;
            if obu_type != OBU_SEQUENCE_HEADER_T
                && obu_type != OBU_TEMPORAL_DELIMITER_T
                && ext
                && let Some(seq) = &self.seq
                && seq.op_idc != 0
            {
                let idc = seq.op_idc;
                let in_t = (idc >> temporal_id) & 1;
                let in_s = (idc >> (spatial_id + 8)) & 1;
                if in_t == 0 || in_s == 0 {
                    continue;
                }
            }
            match obu_type {
                OBU_SEQUENCE_HEADER_T => {
                    let s = SequenceHeader::parse(payload)?;
                    self.seq = Some(s);
                }
                OBU_TEMPORAL_DELIMITER_T => {
                    self.seen_frame_header = false;
                    tu += 1;
                }
                OBU_FRAME_HEADER_T | OBU_REDUNDANT_FRAME_HEADER_T | OBU_FRAME_T => {
                    if self.seen_frame_header {
                        // frame_header_copy(): identical to the active header
                        if obu_type != OBU_FRAME_T {
                            continue;
                        }
                    }
                    let mut r = BitReader::new(payload);
                    let shown = self.frame_header(&mut r, temporal_id, spatial_id)?;
                    if let Some(pic) = shown {
                        out.push((pic, tu, spatial_id));
                        continue;
                    }
                    if obu_type == OBU_FRAME_T {
                        r.byte_align();
                        let rest = &payload[r.byte_pos()..];
                        if let Some(pic) = self.tile_group(rest)? {
                            out.push((pic, tu, spatial_id));
                        }
                    }
                }
                OBU_TILE_GROUP_T => {
                    if let Some(pic) = self.tile_group(payload)? {
                        out.push((pic, tu, spatial_id));
                    }
                }
                OBU_METADATA_T | OBU_TILE_LIST_T => {}
                _ => {}
            }
        }
        // Output policy (7.18.1 note): with scalability, show one frame per temporal unit, the
        // highest spatial layer present.
        let scalable = self.seq.as_ref().is_some_and(|s| s.op_idc != 0);
        if !scalable {
            return Ok(out.into_iter().map(|(p, _, _)| p).collect());
        }
        let mut pics: Vec<Picture> = Vec::new();
        let mut last: Option<(usize, u32)> = None;
        for (p, t, sid) in out {
            match last {
                Some((lt, lsid)) if lt == t => {
                    if sid >= lsid {
                        *pics.last_mut().expect("picture") = p;
                        last = Some((t, sid));
                    }
                }
                _ => {
                    pics.push(p);
                    last = Some((t, sid));
                }
            }
        }
        Ok(pics)
    }

    /// frame_header_obu( ): returns a picture for show_existing_frame.
    fn frame_header(&mut self, r: &mut BitReader, temporal_id: u32, spatial_id: u32) -> Result<Option<Picture>> {
        let seq = self.seq.clone().ok_or(Error::Invalid("frame before sequence header"))?;
        self.seen_frame_header = true;
        let mut st = HeaderState {
            seq: &seq,
            refs: &mut self.refs,
            lf_deltas: self.lf_deltas,
            seg_features: self.seg_features,
            prev_gm_params: self.prev_gm_params,
            current_frame_id: self.current_frame_id,
        };
        let fh = FrameHeader::parse(r, &mut st, temporal_id, spatial_id)?;
        self.lf_deltas = st.lf_deltas;
        self.seg_features = st.seg_features;
        self.prev_gm_params = st.prev_gm_params;
        self.current_frame_id = st.current_frame_id;
        if fh.show_existing_frame {
            self.seen_frame_header = false;
            let idx = fh.frame_to_show_map_idx;
            let rf = self.ref_frames[idx].clone().ok_or(Error::Invalid("show_existing_frame of an empty slot"))?;
            let info = self.refs[idx].clone();
            let pic = self.output_picture(&seq, &rf, info.upscaled_width as usize, info.frame_height as usize, &fh.film_grain);
            if fh.frame_type == KEY_FRAME as u8 {
                // reference frame loading process (7.21) then refresh every slot (7.20)
                self.lf_deltas = info.lf_deltas;
                self.seg_features = info.seg_features;
                for i in 0..NUM_REF_FRAMES {
                    self.refs[i] = info.clone();
                    self.ref_frames[i] = Some(rf.clone());
                }
            }
            return Ok(Some(pic));
        }
        // Encoders exist that code frames slightly larger than the sequence maximum (e.g. a
        // 270-line stream coded as 272 lines); like other decoders we accept them.
        if fh.upscaled_width > 65536 || fh.frame_height > 65536 {
            return Err(Error::Invalid("frame size"));
        }
        let mut fs = FrameState::new(&seq, &fh);
        fs.refs = self.ref_frames.clone();
        fs.ref_info = self.refs.clone();
        if !fh.frame_is_intra {
            for i in 0..REFS_PER_FRAME {
                if fs.refs[fh.ref_frame_idx[i]].is_none() {
                    return Err(Error::Invalid("missing reference frame"));
                }
            }
        }
        if fh.primary_ref_frame != PRIMARY_REF_NONE {
            // load_previous_segment_ids( )
            if let Some(rf) = &self.ref_frames[fh.ref_frame_idx[fh.primary_ref_frame]]
                && fh.seg.enabled
                && rf.mi_cols == fs.mi.cols
                && rf.mi_rows == fs.mi.rows
            {
                fs.prev_segment_ids.copy_from_slice(&rf.segment_ids);
            }
        }
        if fh.use_ref_frame_mvs {
            motion_field_estimation(&mut fs);
        }
        let cdfs = if fh.primary_ref_frame == PRIMARY_REF_NONE {
            Cdfs::new(fh.quant.base_q_idx)
        } else {
            let slot = fh.ref_frame_idx[fh.primary_ref_frame];
            let rf = self.ref_frames[slot].as_ref().ok_or(Error::Invalid("primary reference frame missing"))?;
            let mut c = rf.cdfs.clone();
            c.clear_counts();
            c
        };
        self.cur = Some(Box::new(CurFrame { fs, cdfs, saved_cdfs: None }));
        Ok(None)
    }

    /// tile_group_obu( sz ): returns the output picture when the frame completes and is shown.
    fn tile_group(&mut self, data: &[u8]) -> Result<Option<Picture>> {
        let mut cur = self.cur.take().ok_or(Error::Invalid("tile group without a frame header"))?;
        let (cols, rows, cols_log2, rows_log2, tsb, ctx_tile) = {
            let t = &cur.fs.fh.tile_info;
            (t.cols, t.rows, t.cols_log2, t.rows_log2, t.tile_size_bytes, t.context_update_tile_id as usize)
        };
        let num_tiles = cols * rows;
        let mut r = BitReader::new(data);
        let mut tg_start = 0;
        let mut tg_end = num_tiles - 1;
        if num_tiles > 1 && r.flag()? {
            let bits = cols_log2 + rows_log2;
            tg_start = r.f(bits)? as usize;
            tg_end = r.f(bits)? as usize;
        }
        r.byte_align();
        if tg_end >= num_tiles || tg_start > tg_end {
            return Err(Error::Invalid("tile group range"));
        }
        let mut pos = r.byte_pos();
        for tile_num in tg_start..=tg_end {
            let tile_row = tile_num / cols;
            let tile_col = tile_num % cols;
            let size = if tile_num == tg_end {
                data.len() - pos
            } else {
                let mut v = 0usize;
                for i in 0..tsb as usize {
                    v |= (*data.get(pos + i).ok_or(Error::Truncated)? as usize) << (8 * i);
                }
                pos += tsb as usize;
                v + 1
            };
            let end = pos.checked_add(size).filter(|&e| e <= data.len()).ok_or(Error::Truncated)?;
            let tile_data = &data[pos..end];
            pos = end;
            let cdfs = cur.cdfs.clone();
            cur.fs.intra_frame_y_mode_cdf = DEFAULT_INTRA_FRAME_Y_MODE_CDF;
            let mut td = TileDecoder::new(&mut cur.fs, tile_data, cdfs, tile_row, tile_col);
            td.decode_tile()?;
            if !td.fs.fh.disable_frame_end_update_cdf && tile_num == ctx_tile {
                cur.saved_cdfs = Some(td.cdf);
            }
        }
        if tg_end != num_tiles - 1 {
            self.cur = Some(cur);
            return Ok(None);
        }
        let mut cur = *cur;
        if !cur.fs.fh.disable_frame_end_update_cdf
            && let Some(saved) = cur.saved_cdfs.take()
        {
            cur.cdfs = saved;
        }
        self.seen_frame_header = false;
        self.decode_frame_wrapup(cur)
    }

    fn decode_frame_wrapup(&mut self, cur: CurFrame) -> Result<Option<Picture>> {
        let CurFrame { mut fs, cdfs, .. } = cur;
        crate::postfilter::apply(&mut fs);
        let seq = fs.seq.clone();
        let fh = fs.fh.clone();
        if fh.seg.enabled && !fh.seg.update_map {
            let prev = std::mem::take(&mut fs.prev_segment_ids);
            fs.mi.segment_id.copy_from_slice(&prev);
        }
        let (mf_refs, mf_mvs) = motion_vector_storage(&fs);
        let rf = Arc::new(RefFrame {
            buf: std::mem::take(&mut fs.cur),
            cdfs,
            bit_depth: seq.color.bit_depth,
            subsampling_x: seq.color.subsampling_x,
            subsampling_y: seq.color.subsampling_y,
            segment_ids: std::mem::take(&mut fs.mi.segment_id),
            mi_cols: fs.mi.cols,
            mi_rows: fs.mi.rows,
            film_grain_present: seq.film_grain_params_present,
            color: (seq.color.color_primaries, seq.color.transfer_characteristics, seq.color.matrix_coefficients, seq.color.color_range),
            saved_ref_frames: mf_refs,
            saved_mvs: mf_mvs,
        });
        // reference frame update process (7.20)
        for i in 0..NUM_REF_FRAMES {
            if (fh.refresh_frame_flags >> i) & 1 == 1 {
                let ri = &mut self.refs[i];
                ri.valid = true;
                ri.frame_id = fh.current_frame_id;
                ri.upscaled_width = fh.upscaled_width;
                ri.frame_width = fh.frame_width;
                ri.frame_height = fh.frame_height;
                ri.render_width = fh.render_width;
                ri.render_height = fh.render_height;
                ri.mi_cols = fh.mi_cols;
                ri.mi_rows = fh.mi_rows;
                ri.frame_type = fh.frame_type;
                ri.order_hint = fh.order_hint;
                ri.saved_order_hints = fh.order_hints;
                ri.gm_params = fh.gm_params;
                ri.lf_deltas = fh.lf.deltas;
                ri.seg_features = fh.seg.features;
                ri.grain = fh.film_grain.clone();
                self.ref_frames[i] = Some(rf.clone());
            }
        }
        if fh.show_frame { Ok(Some(self.output_picture(&seq, &rf, fh.upscaled_width as usize, fh.frame_height as usize, &fh.film_grain))) } else { Ok(None) }
    }

    fn output_picture(&self, seq: &SequenceHeader, rf: &RefFrame, w: usize, h: usize, grain: &crate::header::FilmGrainParams) -> Picture {
        let ssx = rf.subsampling_x as usize;
        let ssy = rf.subsampling_y as usize;
        let num_planes = rf.buf.num_planes;
        let mut planes: [Vec<u16>; 3] = Default::default();
        for p in 0..num_planes {
            let (pw, ph) = if p == 0 { (w, h) } else { ((w + ssx) >> ssx, (h + ssy) >> ssy) };
            let src = &rf.buf.planes[p];
            let mut v = Vec::with_capacity(pw * ph);
            for y in 0..ph {
                v.extend_from_slice(&src.row(y)[..pw]);
            }
            planes[p] = v;
        }
        let mut pic = Picture {
            width: w as u32,
            height: h as u32,
            bit_depth: rf.bit_depth,
            subsampling_x: rf.subsampling_x,
            subsampling_y: rf.subsampling_y,
            mono_chrome: num_planes == 1,
            planes,
            color_primaries: rf.color.0,
            transfer_characteristics: rf.color.1,
            matrix_coefficients: rf.color.2,
            full_range: rf.color.3,
        };
        if self.apply_film_grain && seq.film_grain_params_present && grain.apply_grain {
            crate::grain::apply(&mut pic, grain, seq);
        }
        pic
    }
}

/// Motion field motion vector storage process (7.19): (MfRefFrames, MfMvs).
fn motion_vector_storage(fs: &FrameState) -> (Vec<i8>, Vec<[i32; 2]>) {
    let mi = &fs.mi;
    let n = mi.rows * mi.cols;
    let mut refs = vec![-1i8; n];
    let mut mvs = vec![[0i32; 2]; n];
    let fh = &fs.fh;
    for i in 0..n {
        for list in 0..2 {
            let r = mi.ref_frame[i][list];
            if r > INTRA_FRAME as i8 {
                let ref_idx = fh.ref_frame_idx[r as usize - LAST_FRAME];
                let dist = crate::header::relative_dist(&fs.seq, fs.ref_info[ref_idx].order_hint, fh.order_hint);
                if dist < 0 {
                    let m = mi.mv[i][list];
                    let (row, col) = (m.row as i32, m.col as i32);
                    if row.abs() <= REFMVS_LIMIT as i32 && col.abs() <= REFMVS_LIMIT as i32 {
                        refs[i] = r;
                        mvs[i] = [row, col];
                    }
                }
            }
        }
    }
    (refs, mvs)
}

/// Motion field estimation process (7.9).
fn motion_field_estimation(fs: &mut FrameState) {
    let w8 = fs.fh.mi_cols as usize >> 1;
    let h8 = fs.fh.mi_rows as usize >> 1;
    for r in LAST_FRAME..=ALTREF_FRAME {
        fs.motion_field[r] = vec![[crate::mvpred::INVALID_MV, crate::mvpred::INVALID_MV]; w8 * h8];
    }
    let fh = fs.fh.clone();
    let seq = fs.seq.clone();
    let rd = |a: u32, b: u32| crate::header::relative_dist(&seq, a, b);
    let last_idx = fh.ref_frame_idx[0];
    let cur_gold = fh.order_hints[GOLDEN_FRAME];
    let last_alt = fs.ref_info[last_idx].saved_order_hints[ALTREF_FRAME];
    if last_alt != cur_gold {
        project(fs, LAST_FRAME, -1);
    }
    let mut ref_stamp = MFMV_STACK_SIZE as i32 - 2;
    if rd(fh.order_hints[BWDREF_FRAME], fh.order_hint) > 0 && project(fs, BWDREF_FRAME, 1) {
        ref_stamp -= 1;
    }
    if rd(fh.order_hints[ALTREF2_FRAME], fh.order_hint) > 0 && project(fs, ALTREF2_FRAME, 1) {
        ref_stamp -= 1;
    }
    if rd(fh.order_hints[ALTREF_FRAME], fh.order_hint) > 0 && ref_stamp >= 0 && project(fs, ALTREF_FRAME, 1) {
        ref_stamp -= 1;
    }
    if ref_stamp >= 0 {
        project(fs, LAST2_FRAME, -1);
    }
}

fn get_mv_projection(mv: [i32; 2], numerator: i32, denominator: i32) -> [i32; 2] {
    let den = denominator.min(MAX_FRAME_DISTANCE as i32);
    let num = numerator.clamp(-(MAX_FRAME_DISTANCE as i32), MAX_FRAME_DISTANCE as i32);
    let mut out = [0i32; 2];
    for i in 0..2 {
        let scaled = crate::mvpred::round2_signed64(mv[i] as i64 * num as i64 * DIV_MULT[den as usize] as i64, 14);
        out[i] = (scaled as i32).clamp(-(1 << 14) + 1, (1 << 14) - 1);
    }
    out
}

fn project_pos(v8: i32, delta: i32, dst_sign: i32, max8: i32, max_off8: i32) -> Option<i32> {
    let base8 = (v8 >> 3) << 3;
    let offset8 = if delta >= 0 { delta >> (3 + 1 + 2) } else { -((-delta) >> (3 + 1 + 2)) };
    let v = v8 + dst_sign * offset8;
    if v < 0 || v >= max8 || v < base8 - max_off8 || v >= base8 + 8 + max_off8 { None } else { Some(v) }
}

/// Projection process (7.9.2).
fn project(fs: &mut FrameState, src: usize, dst_sign: i32) -> bool {
    let fh = fs.fh.clone();
    let seq = fs.seq.clone();
    let src_idx = fh.ref_frame_idx[src - LAST_FRAME];
    let w8 = fh.mi_cols as i32 >> 1;
    let h8 = fh.mi_rows as i32 >> 1;
    let ri = fs.ref_info[src_idx].clone();
    if ri.mi_rows != fh.mi_rows || ri.mi_cols != fh.mi_cols || ri.frame_type == INTRA_ONLY_FRAME as u8 || ri.frame_type == KEY_FRAME as u8 {
        return false;
    }
    let Some(rf) = fs.refs[src_idx].clone() else { return false };
    let rd = |a: u32, b: u32| crate::header::relative_dist(&seq, a, b);
    let mi_cols = fh.mi_cols as usize;
    for y8 in 0..h8 {
        for x8 in 0..w8 {
            let row = (2 * y8 + 1) as usize;
            let col = (2 * x8 + 1) as usize;
            let src_ref = rf.saved_ref_frames[row * mi_cols + col];
            if src_ref > INTRA_FRAME as i8 {
                let ref_to_cur = rd(fh.order_hints[src], fh.order_hint);
                let ref_offset = rd(fh.order_hints[src], ri.saved_order_hints[src_ref as usize]);
                let pos_valid = ref_to_cur.abs() <= MAX_FRAME_DISTANCE as i32 && ref_offset.abs() <= MAX_FRAME_DISTANCE as i32 && ref_offset > 0;
                if pos_valid {
                    let mv = rf.saved_mvs[row * mi_cols + col];
                    let proj = get_mv_projection(mv, ref_to_cur * dst_sign, ref_offset);
                    let py = project_pos(y8, proj[0], dst_sign, h8, MAX_OFFSET_HEIGHT as i32);
                    let px = project_pos(x8, proj[1], dst_sign, w8, MAX_OFFSET_WIDTH as i32);
                    if let (Some(py), Some(px)) = (py, px) {
                        for dst in LAST_FRAME..=ALTREF_FRAME {
                            let ref_to_dst = rd(fh.order_hint, fh.order_hints[dst]);
                            let pm = get_mv_projection(mv, ref_to_dst, ref_offset);
                            fs.motion_field[dst][(py * w8 + px) as usize] = pm;
                        }
                    }
                }
            }
        }
    }
    true
}
