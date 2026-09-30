//! Clean-room pure-Rust H.264 / AVC decoder, implemented from ITU-T Rec. H.264 (ISO/IEC 14496-10).
//!
//! M2.1: NAL unit, parameter set, slice header and picture order count parsing.

// Constant tables are shared with the macroblock decoding stages that follow.
#![allow(dead_code)]

mod error;
pub mod params;
pub mod slice;
mod tables;

pub use error::{Error, Result};
