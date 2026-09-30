//! Top-level decoder: NAL dispatch, picture boundaries, POC, DPB, output.

use crate::deblock;
use crate::dpb::{Dpb, Output, OutputMeta};
use crate::error::{Error, Result, ensure, unsupported};
use crate::params::{Pps, Sps};
use crate::picture::{Frame, MotionField, Planes};
use crate::slice::{NalHeader, Poc, PocState, SliceHeader, nal_type};
use crate::slicedec::{PicState, SliceDecoder};
use crate::transform::LevelScale;
use crate::{ColorInfo, Picture};
use filmcraft_bitstream::{BitReader, annexb_nals, length_prefixed_nals, unescape_rbsp};
use std::sync::Arc;

struct CurPic {
    pic: PicState,
    first: SliceHeader,
    sps: Arc<Sps>,
    poc: Poc,
    pts: i64,
    key: bool,
    has_mmco5: bool,
}

/// H.264 decoder.
pub struct Decoder {
    spss: Vec<Option<Arc<Sps>>>,
    ppss: Vec<Option<Arc<Pps>>>,
    nal_length_size: Option<usize>,
    dpb: Dpb,
    poc_state: PocState,
    prev_ref_frame_num: u32,
    cur: Option<CurPic>,
    next_id: u32,
    active_sps: Option<Arc<Sps>>,
    ls_cache: Vec<(Arc<Pps>, Arc<LevelScale>)>,
    out: Vec<Picture>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Is `sh` the first slice of a new primary coded picture relative to `prev` (7.4.1.2.4)?
fn is_new_picture(prev: &SliceHeader, sh: &SliceHeader, sps: &Sps) -> bool {
    if sh.frame_num != prev.frame_num
        || sh.pps_id != prev.pps_id
        || sh.field_pic != prev.field_pic
        || sh.bottom_field != prev.bottom_field
        || (sh.nal_ref_idc == 0) != (prev.nal_ref_idc == 0)
        || sh.idr != prev.idr
        || (sh.idr && prev.idr && sh.idr_pic_id != prev.idr_pic_id)
    {
        return true;
    }
    if sps.pic_order_cnt_type == 0 && (sh.pic_order_cnt_lsb != prev.pic_order_cnt_lsb || sh.delta_pic_order_cnt_bottom != prev.delta_pic_order_cnt_bottom) {
        return true;
    }
    if sps.pic_order_cnt_type == 1 && sh.delta_pic_order_cnt != prev.delta_pic_order_cnt {
        return true;
    }
    false
}

impl Decoder {
    pub fn new() -> Self {
        crate::cavlc::init_tables();
        Decoder {
            spss: vec![None; 32],
            ppss: vec![None; 256],
            nal_length_size: None,
            dpb: Dpb::new(),
            poc_state: PocState::default(),
            prev_ref_frame_num: 0,
            cur: None,
            next_id: 1,
            active_sps: None,
            ls_cache: Vec::new(),
            out: Vec::new(),
        }
    }

    /// Configure from an `avcC` (AVCDecoderConfigurationRecord) box payload.
    pub fn from_avcc(avcc: &[u8]) -> Result<Self> {
        let mut d = Decoder::new();
        ensure!(avcc.len() >= 7, "avcC too short");
        ensure!(avcc[0] == 1, "unsupported avcC version {}", avcc[0]);
        d.nal_length_size = Some((avcc[4] & 3) as usize + 1);
        let mut pos = 5;
        let read_sets = |pos: &mut usize, count: usize, d: &mut Decoder| -> Result<()> {
            for _ in 0..count {
                ensure!(*pos + 2 <= avcc.len(), "avcC truncated");
                let len = u16::from_be_bytes([avcc[*pos], avcc[*pos + 1]]) as usize;
                *pos += 2;
                ensure!(*pos + len <= avcc.len(), "avcC truncated");
                d.handle_nal(&avcc[*pos..*pos + len], 0)?;
                *pos += len;
            }
            Ok(())
        };
        let nsps = (avcc[pos] & 0x1f) as usize;
        pos += 1;
        read_sets(&mut pos, nsps, &mut d)?;
        ensure!(pos < avcc.len(), "avcC truncated");
        let npps = avcc[pos] as usize;
        pos += 1;
        read_sets(&mut pos, npps, &mut d)?;
        Ok(d)
    }

    /// Length of NAL length prefixes (from avcC), or None for Annex-B input.
    pub fn nal_length_size(&self) -> Option<usize> {
        self.nal_length_size
    }

    /// Decode one access unit. Returns pictures that became ready for output, in output order.
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let nals = match self.nal_length_size {
            Some(n) => length_prefixed_nals(data, n)?,
            None => annexb_nals(data),
        };
        let mut result = Ok(());
        for nal in nals {
            if let Err(e) = self.handle_nal(nal, pts) {
                result = Err(e);
                break;
            }
        }
        // Finish a complete picture at the end of the access unit.
        if self.cur.as_ref().is_some_and(|c| c.pic.decoded_mbs >= c.pic.mbs.len()) {
            self.finish_picture();
        }
        result?;
        Ok(std::mem::take(&mut self.out))
    }

    /// Output all remaining pictures.
    pub fn flush(&mut self) -> Vec<Picture> {
        if self.cur.is_some() {
            self.finish_picture();
        }
        let mut outs = Vec::new();
        self.dpb.flush(&mut outs);
        self.emit(outs);
        std::mem::take(&mut self.out)
    }

    fn handle_nal(&mut self, nal: &[u8], pts: i64) -> Result<()> {
        if nal.is_empty() {
            return Ok(());
        }
        let hdr = NalHeader::parse(nal[0])?;
        match hdr.nal_unit_type {
            nal_type::SPS => {
                let sps = Sps::parse(&unescape_rbsp(&nal[1..]))?;
                let id = sps.id as usize;
                self.spss[id] = Some(Arc::new(sps));
            }
            nal_type::PPS => {
                let rbsp = unescape_rbsp(&nal[1..]);
                let pps = Pps::parse(&rbsp, &self.spss_plain())?;
                let id = pps.id as usize;
                self.ppss[id] = Some(Arc::new(pps));
            }
            nal_type::SLICE | nal_type::IDR => self.handle_slice(nal, hdr, pts)?,
            nal_type::SLICE_DPA | nal_type::SLICE_DPB | nal_type::SLICE_DPC => {
                return unsupported("data partitioning (Extended profile)");
            }
            nal_type::END_SEQ | nal_type::END_STREAM if self.cur.is_some() => self.finish_picture(),
            // SEI, AUD, filler, SPS extension, prefix NAL, subset SPS, auxiliary and extension slices: skipped.
            _ => {}
        }
        Ok(())
    }

    fn spss_plain(&self) -> Vec<Option<Sps>> {
        self.spss.iter().map(|s| s.as_ref().map(|s| (**s).clone())).collect()
    }

    fn level_scale(&mut self, pps: &Arc<Pps>) -> Arc<LevelScale> {
        if let Some((_, ls)) = self.ls_cache.iter().find(|(p, _)| Arc::ptr_eq(p, pps)) {
            return ls.clone();
        }
        let ls: Arc<LevelScale> = Arc::from(LevelScale::new(&pps.scaling));
        self.ls_cache.retain(|(p, _)| self.ppss.iter().flatten().any(|q| Arc::ptr_eq(p, q)));
        self.ls_cache.push((pps.clone(), ls.clone()));
        ls
    }

    fn handle_slice(&mut self, nal: &[u8], hdr: NalHeader, pts: i64) -> Result<()> {
        let rbsp = unescape_rbsp(&nal[1..]);
        let ppss = &self.ppss;
        let spss = &self.spss;
        let mut found: Option<(Arc<Pps>, Arc<Sps>)> = None;
        let (sh, _, _) = SliceHeader::parse(&rbsp, hdr, |id| {
            let pps = ppss[id as usize].as_ref().ok_or_else(|| Error::MissingParameterSet(format!("PPS {id}")))?;
            let sps = spss[pps.sps_id as usize].as_ref().ok_or_else(|| Error::MissingParameterSet(format!("SPS {}", pps.sps_id)))?;
            found = Some((pps.clone(), sps.clone()));
            Ok((&**pps, &**sps))
        })?;
        let (pps, sps) = found.expect("lookup succeeded");
        sps.check_supported()?;
        if pps.num_slice_groups > 1 {
            return unsupported("slice groups (FMO)");
        }
        if sh.field_pic {
            return unsupported("field pictures");
        }
        if matches!(sh.slice_type, crate::slice::SliceType::Sp | crate::slice::SliceType::Si) {
            return unsupported("SP/SI slices");
        }
        if sh.redundant_pic_cnt > 0 {
            return Ok(()); // redundant slices are ignored
        }
        let new_pic = match &self.cur {
            None => true,
            Some(c) => is_new_picture(&c.first, &sh, &sps) || (sh.first_mb_in_slice == 0 && c.pic.decoded_mbs > 0) || !Arc::ptr_eq(&c.sps, &sps),
        };
        if new_pic {
            if self.cur.is_some() {
                self.finish_picture();
            }
            self.start_picture(&sh, &sps, pts)?;
        }
        let ls = self.level_scale(&pps);
        let cur = self.cur.as_mut().expect("picture started");
        if sh.has_mmco5() {
            cur.has_mmco5 = true;
        }
        let refs = self.dpb.build_ref_lists(&sh, cur.poc.frame(), sps.max_frame_num())?;
        if !sh.slice_type.is_intra() {
            ensure!(!refs[0].is_empty(), "no reference pictures available for inter slice");
            if sh.slice_type.is_b() {
                ensure!(!refs[1].is_empty(), "empty RefPicList1 in B slice");
            }
        }
        let mut sd = SliceDecoder::new(&sh, &pps, &sps, &mut cur.pic, &refs, &ls)?;
        if pps.entropy_coding_mode {
            sd.decode_cabac(&rbsp)?;
        } else {
            let mut r = BitReader::new(&rbsp);
            r.seek_bits(sh.header_bits);
            sd.decode_cavlc(&mut r)?;
        }
        Ok(())
    }

    fn start_picture(&mut self, sh: &SliceHeader, sps: &Arc<Sps>, pts: i64) -> Result<()> {
        // Activate SPS; a resolution change flushes the DPB.
        let changed = match &self.active_sps {
            None => true,
            Some(a) => a.width() != sps.width() || a.height() != sps.height() || a.max_dpb_frames() != sps.max_dpb_frames(),
        };
        if changed && self.active_sps.is_some() {
            let mut outs = Vec::new();
            self.dpb.flush(&mut outs);
            self.emit(outs);
            self.dpb.entries.clear();
        }
        self.active_sps = Some(sps.clone());
        self.dpb.capacity = sps.max_dpb_frames();
        self.dpb.max_reorder = sps.max_num_reorder_frames().min(self.dpb.capacity);
        let mb_w = sps.pic_width_in_mbs as usize;
        let mb_h = sps.frame_height_in_mbs() as usize;
        if sh.idr {
            let mut outs = Vec::new();
            self.dpb.idr(sh.no_output_of_prior_pics, &mut outs);
            self.emit(outs);
            self.prev_ref_frame_num = 0;
        } else {
            if self.dpb.entries.iter().all(|e| e.mark == crate::dpb::RefMark::Unused) && !sh.slice_type.is_intra() {
                // Stream starts without an IDR: nothing to reference; decoding will conceal.
            }
            let max = sps.max_frame_num();
            if sh.frame_num != self.prev_ref_frame_num && sh.frame_num != (self.prev_ref_frame_num + 1) % max {
                // frame_num gap (8.2.5.2)
                let (w, h) = (mb_w * 16, mb_h * 16);
                let meta = Arc::new(output_meta(sps, pts, false));
                let next_id = &mut self.next_id;
                let poc_state = &mut self.poc_state;
                let prev_last = self.dpb.entries.iter().filter(|e| !e.non_existing).max_by_key(|e| e.frame.id).map(|e| e.frame.clone());
                let mut make = |_fnum: u32| {
                    let id = *next_id;
                    *next_id += 1;
                    let planes = match &prev_last {
                        Some(f) => f.planes.clone(),
                        None => Planes::gray(w, h),
                    };
                    let frame = Arc::new(Frame { id, poc: 0, planes, motion: MotionField::default() });
                    (frame, meta.clone())
                };
                let mut upd = |fnum: u32| poc_state.update_gap_frame(fnum, sps);
                self.dpb.fill_frame_num_gap(self.prev_ref_frame_num, sh.frame_num, max, sps.max_num_ref_frames as usize, &mut make, &mut upd);
                self.prev_ref_frame_num = (sh.frame_num + max - 1) % max;
            }
        }
        let poc = self.poc_state.compute(sh, sps);
        let pic = PicState::new(mb_w, mb_h, poc.frame());
        self.cur = Some(CurPic { pic, first: sh.clone(), sps: sps.clone(), poc, pts, key: sh.idr, has_mmco5: false });
        Ok(())
    }
}

fn output_meta(sps: &Sps, pts: i64, key: bool) -> OutputMeta {
    {
        let vui = sps.vui.clone().unwrap_or_default();
        OutputMeta {
            pts,
            key,
            crop: sps.crop_rect(),
            full_range: vui.full_range,
            colour_primaries: vui.colour_primaries,
            transfer_characteristics: vui.transfer_characteristics,
            matrix_coefficients: vui.matrix_coefficients,
            sar: vui.sar,
        }
    }
}

impl Decoder {
    fn finish_picture(&mut self) {
        let Some(mut cur) = self.cur.take() else { return };
        deblock::deblock_picture(&mut cur.pic);
        // motion field for co-located use
        let n = cur.pic.mbs.len();
        let mut motion = MotionField {
            mv: [vec![[0; 2]; n * 16], vec![[0; 2]; n * 16]],
            ref_idx: [vec![-1; n * 4], vec![-1; n * 4]],
            ref_id: [vec![u32::MAX; n * 4], vec![u32::MAX; n * 4]],
            intra: vec![true; n],
        };
        for (i, st) in cur.pic.mbs.iter().enumerate() {
            if st.slice_num == u32::MAX {
                continue;
            }
            motion.intra[i] = st.kind.is_intra();
            let sl = &cur.pic.slices[st.slice_num as usize];
            for l in 0..2 {
                motion.mv[l][i * 16..i * 16 + 16].copy_from_slice(&st.mv[l]);
                for b in 0..4 {
                    let r = st.ref_idx[l][b];
                    motion.ref_idx[l][i * 4 + b] = r;
                    if r >= 0 {
                        motion.ref_id[l][i * 4 + b] = sl.ref_ids[l].get(r as usize).copied().unwrap_or(u32::MAX);
                    }
                }
            }
        }
        let mut poc = cur.poc.frame();
        let sh = cur.first.clone();
        if cur.has_mmco5 {
            // tempPicOrderCnt adjustment (8.2.1): the picture's POC becomes relative to itself.
            poc -= cur.poc.frame();
        }
        let id = self.next_id;
        self.next_id += 1;
        let frame = Arc::new(Frame { id, poc, planes: cur.pic.planes, motion });
        let meta = Arc::new(output_meta(&cur.sps, cur.pts, cur.key));
        let sh_eff = sh.clone();
        self.poc_state.update(&sh_eff, &cur.poc);
        if sh.nal_ref_idc != 0 {
            self.prev_ref_frame_num = if cur.has_mmco5 { 0 } else { sh.frame_num };
        }
        let mut outs = Vec::new();
        self.dpb.store_picture(&sh_eff, frame, poc, cur.sps.max_frame_num(), cur.sps.max_num_ref_frames as usize, meta, &mut outs);
        self.emit(outs);
    }

    fn emit(&mut self, outs: Vec<Output>) {
        for o in outs {
            self.out.push(make_picture(&o));
        }
    }
}

fn make_picture(o: &Output) -> Picture {
    let p = &o.frame.planes;
    let (cx, cy, cw, ch) = o.meta.crop;
    let (cx, cy, cw, ch) = (cx as usize, cy as usize, cw as usize, ch as usize);
    let mut y = Vec::with_capacity(cw * ch);
    for r in cy..cy + ch {
        y.extend_from_slice(&p.y[r * p.width + cx..r * p.width + cx + cw]);
    }
    let (ccx, ccy, ccw, cch) = (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2));
    let mut u = Vec::with_capacity(ccw * cch);
    let mut v = Vec::with_capacity(ccw * cch);
    for r in ccy..ccy + cch {
        u.extend_from_slice(&p.cb[r * p.cwidth + ccx..r * p.cwidth + ccx + ccw]);
        v.extend_from_slice(&p.cr[r * p.cwidth + ccx..r * p.cwidth + ccx + ccw]);
    }
    Picture {
        width: cw as u32,
        height: ch as u32,
        chroma_width: ccw as u32,
        chroma_height: cch as u32,
        y,
        u,
        v,
        y_stride: cw,
        uv_stride: ccw,
        pts: o.meta.pts,
        poc: o.poc,
        key: o.meta.key,
        color: ColorInfo {
            full_range: o.meta.full_range,
            primaries: o.meta.colour_primaries,
            transfer: o.meta.transfer_characteristics,
            matrix: o.meta.matrix_coefficients,
        },
        sar: o.meta.sar,
    }
}
