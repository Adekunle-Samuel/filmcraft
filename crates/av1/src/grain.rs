//! Film grain synthesis (7.18.3).

use crate::Picture;
use crate::header::{FilmGrainParams, SequenceHeader};

pub(crate) fn apply(pic: &mut Picture, g: &FilmGrainParams, seq: &SequenceHeader) {
    let _ = (pic, g, seq);
}
