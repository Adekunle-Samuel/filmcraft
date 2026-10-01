//! In-loop post filters run by decode_frame_wrapup: loop filter (7.14), CDEF (7.15),
//! super-resolution upscaling (7.16) and loop restoration (7.17).

use crate::state::FrameState;

pub(crate) fn apply(fs: &mut FrameState) {
    let _ = fs;
}
