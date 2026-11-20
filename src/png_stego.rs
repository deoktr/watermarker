//! LSB steganography in PNG pixel samples. The image is losslessly encoded,
//! so the payload survives any bit-exact copy of the delivered file.

use crate::stego;

/// Number of usable carrier slots: one LSB per R, G and B sample.
pub fn capacity(width: usize, height: usize) -> usize {
    width * height * 3
}

fn offset(channels: usize, slot: usize) -> usize {
    let pixel = slot / 3;
    let channel = slot % 3;
    pixel * channels + channel
}

pub fn embed(
    buffer: &mut [u8],
    width: usize,
    height: usize,
    channels: usize,
    frame: &[u8],
    seed: u64,
) -> Result<usize, String> {
    let slots = capacity(width, height);
    stego::embed_with(frame, slots, seed, |slot, bit| {
        let at = offset(channels, slot);
        buffer[at] = (buffer[at] & 0xfe) | bit;
    })?;
    Ok(slots)
}

pub fn extract(
    buffer: &[u8],
    width: usize,
    height: usize,
    channels: usize,
    seed: u64,
) -> Result<Vec<u8>, String> {
    let slots = capacity(width, height);
    stego::extract_with(slots, seed, |slot| buffer[offset(channels, slot)] & 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_changes_only_rgb_lsb() {
        let (width, height, channels) = (64usize, 64usize, 4usize);
        let mut buffer: Vec<u8> = (0..width * height * channels)
            .map(|index| (index.wrapping_mul(37) % 251) as u8)
            .collect();
        let before = buffer.clone();
        let frame = crate::stego::frame(b"png-payload").unwrap();

        embed(&mut buffer, width, height, channels, &frame, 7).unwrap();
        assert_eq!(
            extract(&buffer, width, height, channels, 7).unwrap(),
            b"png-payload"
        );
        assert!(extract(&buffer, width, height, channels, 8).is_err());

        for (old, new) in before.iter().zip(&buffer) {
            assert!(old ^ new <= 1);
        }
        for pixel in 0..width * height {
            assert_eq!(before[pixel * channels + 3], buffer[pixel * channels + 3]);
        }
    }
}
