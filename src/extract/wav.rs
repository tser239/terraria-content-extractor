//! Minimal RIFF/WAVE writers.
//!
//! Two formats are produced:
//! * plain PCM (used for the `SoundEffect` XNBs, which embed a WAVEFORMATEX block)
//! * Microsoft ADPCM (used for the music tracks in `Wave Bank.xwb`)

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_i16(out: &mut Vec<u8>, value: i16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(id);
    push_u32(out, data.len() as u32);
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
}

fn finish(mut out: Vec<u8>) -> Vec<u8> {
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    out
}

fn header(capacity: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(b"RIFF");
    push_u32(&mut out, 0);
    out.extend_from_slice(b"WAVE");
    out
}

/// Wrap raw PCM samples in a WAV container.
///
/// `format` is the WAVEFORMATEX payload exactly as stored in the XNB (18 bytes for
/// the Terraria sound effects, including `cbSize = 0`).
pub fn pcm(format: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = header(12 + 8 + format.len() + 8 + data.len() + 1);
    push_chunk(&mut out, b"fmt ", format);
    push_chunk(&mut out, b"data", data);
    finish(out)
}

/// Microsoft ADPCM coefficients required by `wFormatTag = 2`.
const ADPCM_COEFFICIENTS: [(i16, i16); 7] = [
    (256, 0),
    (512, -256),
    (0, 0),
    (192, 64),
    (240, 0),
    (460, -208),
    (392, -232),
];

/// Wrap an MS ADPCM stream in a WAV container with the extra `fact` chunk and
/// coefficient table that decoders expect.
pub fn ms_adpcm(
    sample_rate: u32,
    channels: u16,
    block_align: u16,
    samples_per_block: u16,
    sample_count: u32,
    data: &[u8],
) -> Vec<u8> {
    debug_assert!(channels > 0);
    debug_assert!(samples_per_block > 0);

    let average_bytes_per_second =
        (sample_rate as u64 * block_align as u64 / samples_per_block.max(1) as u64) as u32;

    let mut format = Vec::with_capacity(50);
    push_u16(&mut format, 2); // WAVE_FORMAT_ADPCM
    push_u16(&mut format, channels);
    push_u32(&mut format, sample_rate);
    push_u32(&mut format, average_bytes_per_second);
    push_u16(&mut format, block_align);
    push_u16(&mut format, 4); // bits per sample
    push_u16(&mut format, 32); // cbSize
    push_u16(&mut format, samples_per_block);
    push_u16(&mut format, ADPCM_COEFFICIENTS.len() as u16);
    for (coef1, coef2) in ADPCM_COEFFICIENTS {
        push_i16(&mut format, coef1);
        push_i16(&mut format, coef2);
    }

    let mut fact = Vec::with_capacity(4);
    push_u32(&mut fact, sample_count);

    let mut out = header(12 + 8 + format.len() + 8 + fact.len() + 8 + data.len() + 1);
    push_chunk(&mut out, b"fmt ", &format);
    push_chunk(&mut out, b"fact", &fact);
    push_chunk(&mut out, b"data", data);
    finish(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_chunk_sizes_are_consistent() {
        let fmt = [
            1u8, 0, 1, 0, 0x44, 0xac, 0, 0, 0x88, 0x58, 1, 0, 2, 0, 16, 0, 0, 0,
        ];
        let data = [0u8; 7]; // odd size, must be padded
        let wav = pcm(&fmt, &data);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let riff_size = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff_size, wav.len() - 8);
        assert_eq!(wav.len() % 2, 0);
    }

    #[test]
    fn adpcm_header_lengths() {
        let wav = ms_adpcm(44_100, 2, 140, 128, 1000, &[0u8; 16]);
        // 12 byte header + fmt (8 + 50) + fact (8 + 4) + data (8 + 16)
        assert_eq!(wav.len(), 12 + 58 + 12 + 24);
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(wav[16..20].try_into().unwrap()), 50);
    }
}
