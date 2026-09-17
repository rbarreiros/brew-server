//! ITU-T G.711 mu-law (PCMU, RTP payload type 0) and A-law (PCMA, payload
//! type 8) companding, per the standard bit-exact algorithm. Pure Rust, no
//! external codec needed: this is what SIP legs are steered to (see
//! `sip::media::negotiate_payloads`), so it is the other half of the
//! ACELP<->G.711 transcoder alongside `acelp`.

const ULAW_BIAS: i32 = 0x84;
const ULAW_CLIP: i32 = 32635;
/// Upper bound (inclusive) of each of the 8 mu-law segments, on the
/// bias-added magnitude.
const ULAW_SEG_END: [i32; 8] = [0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF, 0x3FFF, 0x7FFF];

fn search(val: i32, table: &[i32]) -> usize {
    table.iter().position(|&end| val <= end).unwrap_or(table.len())
}

pub fn ulaw_encode(pcm: i16) -> u8 {
    let mut sample = pcm as i32;
    let sign: u8 = if sample < 0 { sample = -sample; 0x80 } else { 0 };
    if sample > ULAW_CLIP { sample = ULAW_CLIP; }
    sample += ULAW_BIAS;
    let exponent = search(sample, &ULAW_SEG_END) as i32;
    let mantissa = (sample >> (exponent + 3)) & 0x0F;
    !(sign | ((exponent as u8) << 4) | mantissa as u8)
}

pub fn ulaw_decode(byte: u8) -> i16 {
    let byte = !byte;
    let sign = byte & 0x80;
    let exponent = ((byte >> 4) & 0x07) as i32;
    let mantissa = (byte & 0x0F) as i32;
    let mut sample = (mantissa << 3) + ULAW_BIAS;
    sample <<= exponent;
    sample -= ULAW_BIAS;
    (if sign != 0 { -sample } else { sample }) as i16
}

/// Upper bound (inclusive) of each of the 8 A-law segments, on the
/// (magnitude >> 3) value.
const ALAW_SEG_END: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];

pub fn alaw_encode(pcm: i16) -> u8 {
    let mut sample = (pcm as i32) >> 3;
    let mask: u8 = if sample >= 0 { 0xD5 } else { sample = -sample - 1; 0x55 };
    let seg = search(sample, &ALAW_SEG_END);
    if seg >= 8 {
        0x7F ^ mask
    } else {
        let mut aval = (seg as u8) << 4;
        aval |= if seg < 2 { (sample >> 1) & 0x0F } else { (sample >> seg) & 0x0F } as u8;
        aval ^ mask
    }
}

pub fn alaw_decode(byte: u8) -> i16 {
    let byte = byte ^ 0x55;
    let sign = byte & 0x80;
    let seg = ((byte & 0x70) >> 4) as i32;
    let mantissa = (byte & 0x0F) as i32;

    let mut sample = if seg == 0 {
        (mantissa << 4) + 8
    } else {
        ((mantissa << 4) + 0x108) << (seg - 1)
    };
    if sign == 0 { sample = -sample; }
    sample as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulaw_round_trip_is_lossy_but_close() {
        for pcm in [0i16, 100, -100, 3000, -3000, 32000, -32000] {
            let out = ulaw_decode(ulaw_encode(pcm));
            assert!((out as i32 - pcm as i32).abs() < 1200, "pcm={pcm} out={out}");
        }
    }

    #[test]
    fn alaw_round_trip_is_lossy_but_close() {
        for pcm in [0i16, 100, -100, 3000, -3000, 32000, -32000] {
            let out = alaw_decode(alaw_encode(pcm));
            assert!((out as i32 - pcm as i32).abs() < 1200, "pcm={pcm} out={out}");
        }
    }

    #[test]
    fn ulaw_silence_is_stable() {
        assert_eq!(ulaw_decode(ulaw_encode(0)), 0);
    }

    #[test]
    fn alaw_silence_is_near_zero() {
        // A-law's segment-0 rounding means PCM 0 decodes to +-8, not exactly
        // 0 -- expected per the ITU algorithm, not a bug.
        assert!(alaw_decode(alaw_encode(0)).abs() <= 8);
    }
}
