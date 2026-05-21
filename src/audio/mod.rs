//! Audio playback. One long-lived engine thread owns one output device's
//! `rodio` stream + sink (they are `!Send`, so they never leave that thread).

pub mod device;
pub mod engine;
