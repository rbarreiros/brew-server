//! Safe wrapper over the ETSI EN 300 395-2 reference ACELP codec
//! (vendored in `third_party/tetra-codec/`, compiled by `build.rs`).
//!
//! One frame is `ACELP_PCM_SAMPLES` (240) 16-bit PCM samples @ 8kHz in,
//! `ACELP_CODED_FRAME_BYTES` (18) packed bytes out, and vice versa.

use crate::protocol::{ACELP_CODED_FRAME_BYTES, ACELP_PCM_SAMPLES};
use std::os::raw::c_int;

#[allow(non_camel_case_types)]
#[repr(C)]
struct tetra_codec {
    _private: [u8; 0],
}

extern "C" {
    fn tetra_encoder_create() -> *mut tetra_codec;
    fn tetra_decoder_create() -> *mut tetra_codec;
    fn tetra_codec_destroy(st: *mut tetra_codec);
    fn tetra_encode(st: *mut tetra_codec, pcm: *const i16, coded: *mut u8);
    fn tetra_decode(st: *mut tetra_codec, coded: *const u8, pcm: *mut i16, bfi: c_int);
}

/// Encodes 240 PCM samples into 18 bytes of ACELP-coded speech. Owns its own
/// codec state (the reference implementation is stateful across frames, e.g.
/// for LPC prediction), so one instance must persist for the life of a call.
pub struct AcelpEncoder(*mut tetra_codec);

// The codec state is only ever touched from the single transcoder task that
// owns this encoder; it is moved, not shared, across await points.
unsafe impl Send for AcelpEncoder {}

impl AcelpEncoder {
    pub fn new() -> Self {
        Self(unsafe { tetra_encoder_create() })
    }

    /// Encodes exactly `ACELP_PCM_SAMPLES` PCM samples into `ACELP_CODED_FRAME_BYTES`.
    pub fn encode(&mut self, pcm: &[i16; ACELP_PCM_SAMPLES]) -> [u8; ACELP_CODED_FRAME_BYTES] {
        let mut coded = [0u8; ACELP_CODED_FRAME_BYTES];
        unsafe { tetra_encode(self.0, pcm.as_ptr(), coded.as_mut_ptr()) };
        coded
    }
}

impl Default for AcelpEncoder {
    fn default() -> Self { Self::new() }
}

impl Drop for AcelpEncoder {
    fn drop(&mut self) {
        unsafe { tetra_codec_destroy(self.0) };
    }
}

/// Decodes 18 bytes of ACELP-coded speech into 240 PCM samples.
pub struct AcelpDecoder(*mut tetra_codec);

unsafe impl Send for AcelpDecoder {}

impl AcelpDecoder {
    pub fn new() -> Self {
        Self(unsafe { tetra_decoder_create() })
    }

    /// Decodes one frame. `bad_frame` marks a lost/errored frame (bfi flag),
    /// letting the codec's error concealment run instead of decoding garbage.
    pub fn decode(&mut self, coded: &[u8; ACELP_CODED_FRAME_BYTES], bad_frame: bool) -> [i16; ACELP_PCM_SAMPLES] {
        let mut pcm = [0i16; ACELP_PCM_SAMPLES];
        unsafe { tetra_decode(self.0, coded.as_ptr(), pcm.as_mut_ptr(), bad_frame as c_int) };
        pcm
    }
}

impl Default for AcelpDecoder {
    fn default() -> Self { Self::new() }
}

impl Drop for AcelpDecoder {
    fn drop(&mut self) {
        unsafe { tetra_codec_destroy(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_silence_without_crashing() {
        let mut enc = AcelpEncoder::new();
        let mut dec = AcelpDecoder::new();
        let pcm = [0i16; ACELP_PCM_SAMPLES];
        let coded = enc.encode(&pcm);
        let out = dec.decode(&coded, false);
        assert_eq!(out.len(), ACELP_PCM_SAMPLES);
    }

    #[test]
    fn round_trips_a_tone() {
        let mut enc = AcelpEncoder::new();
        let mut dec = AcelpDecoder::new();
        let mut pcm = [0i16; ACELP_PCM_SAMPLES];
        for (i, s) in pcm.iter_mut().enumerate() {
            *s = ((i as f32 * 0.1).sin() * 8000.0) as i16;
        }
        let coded = enc.encode(&pcm);
        let out = dec.decode(&coded, false);
        // Lossy speech codec: just assert it produced non-trivial output, not
        // bit-exactness against the input.
        assert!(out.iter().any(|&s| s != 0));
    }
}
