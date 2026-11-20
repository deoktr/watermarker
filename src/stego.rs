//! Shared steganography primitives: payload framing, keyed slot permutation,
//! and repeated-bit embedding/extraction helpers.

pub const MAGIC: &[u8; 4] = b"WMS1";
pub const VERSION: u8 = 1;
/// How many physical slots carry one logical bit (majority vote).
pub const REP: usize = 3;
/// Header: magic(4) + version(1) + len(2) + crc32(4).
pub const HEADER_LEN: usize = 11;
pub const MAX_PAYLOAD: usize = 8192;

/// Wrap a payload in the self-describing, checksummed frame.
pub fn frame(payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() > MAX_PAYLOAD {
        return Err(format!(
            "hidden payload is {} bytes, maximum is {}",
            payload.len(),
            MAX_PAYLOAD
        ));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Validate a recovered frame and return the payload.
pub fn unframe(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < HEADER_LEN {
        return Err("no hidden payload (frame too short)".into());
    }
    if &data[0..4] != MAGIC {
        return Err("no hidden payload (magic mismatch)".into());
    }
    if data[4] != VERSION {
        return Err(format!("unsupported hidden payload version {}", data[4]));
    }
    let len = u16::from_le_bytes([data[5], data[6]]) as usize;
    if len > MAX_PAYLOAD {
        return Err("hidden payload length is invalid".into());
    }
    if data.len() < HEADER_LEN + len {
        return Err("no hidden payload (truncated frame)".into());
    }
    let crc = u32::from_le_bytes([data[7], data[8], data[9], data[10]]);
    let payload = &data[HEADER_LEN..HEADER_LEN + len];
    if crc32fast::hash(payload) != crc {
        return Err("hidden payload found but checksum is invalid (image was modified)".into());
    }
    Ok(payload.to_vec())
}

pub fn needed_slots(frame_len: usize) -> usize {
    frame_len * 8 * REP
}

pub fn bit_of(data: &[u8], bit: usize) -> u8 {
    (data[bit / 8] >> (7 - (bit % 8))) & 1
}

pub fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len() / 8];
    for (i, &b) in bits.iter().enumerate() {
        if b != 0 {
            out[i / 8] |= 1 << (7 - (i % 8));
        }
    }
    out
}

/// Embed a frame by writing each logical bit into three physical slots.
pub fn embed_with<F>(frame: &[u8], slots: usize, seed: u64, mut write: F) -> Result<(), String>
where
    F: FnMut(usize, u8),
{
    let need = needed_slots(frame.len());
    if slots < need {
        return Err(format!(
            "image has only {} usable stego slots, {} required for a {} byte payload",
            slots,
            need,
            frame.len() - HEADER_LEN
        ));
    }
    let total_bits = frame.len() * 8;
    for bit in 0..total_bits {
        let value = bit_of(frame, bit);
        for copy in 0..REP {
            write(permute((3 * bit + copy) as u32, slots as u32, seed) as usize, value);
        }
    }
    Ok(())
}

/// Extract a frame by majority-voting three physical slots per logical bit.
pub fn extract_with<F>(slots: usize, seed: u64, read: F) -> Result<Vec<u8>, String>
where
    F: Fn(usize) -> u8,
{
    if slots < needed_slots(HEADER_LEN) {
        return Err("no hidden payload (not enough capacity in this image)".into());
    }

    let mut header_bits = Vec::with_capacity(HEADER_LEN * 8);
    let mut bit_index = 0usize;
    while header_bits.len() < HEADER_LEN * 8 {
        let mut votes = 0u8;
        for copy in 0..REP {
            votes += read(permute((3 * bit_index + copy) as u32, slots as u32, seed) as usize);
        }
        header_bits.push(u8::from(votes >= 2));
        bit_index += 1;
    }
    let header = bits_to_bytes(&header_bits);
    if &header[0..4] != MAGIC {
        return Err("no hidden payload found (magic mismatch)".into());
    }
    if header[4] != VERSION {
        return Err(format!("unsupported hidden payload version {}", header[4]));
    }
    let len = u16::from_le_bytes([header[5], header[6]]) as usize;
    if len > MAX_PAYLOAD {
        return Err("hidden payload length is invalid".into());
    }
    let total_bytes = HEADER_LEN + len;
    let total_bits = total_bytes * 8;
    if slots < needed_slots(total_bytes) {
        return Err("hidden payload is truncated (capacity mismatch)".into());
    }

    let mut all_bits = header_bits;
    while all_bits.len() < total_bits {
        let mut votes = 0u8;
        for copy in 0..REP {
            votes += read(permute((3 * bit_index + copy) as u32, slots as u32, seed) as usize);
        }
        all_bits.push(u8::from(votes >= 2));
        bit_index += 1;
    }
    let bytes = bits_to_bytes(&all_bits);
    unframe(&bytes)
}

/// 64-bit keyed pseudo-random value used for ids and permutation seeds.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

pub fn seed_from_key(key: &str) -> u64 {
    splitmix64(fnv1a(key.as_bytes()) ^ 0x5741_5445_524d_4152)
}

pub fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Format-preserving keyed permutation of `0..n` via a 4-round Feistel network
/// and cycle walking over the next power-of-two-ish domain (so acceptance is
/// at least ~1/4 instead of n/2^32).
pub fn permute(x: u32, n: u32, seed: u64) -> u32 {
    if n <= 1 {
        return 0;
    }
    debug_assert!(x < n);
    let bits = 32 - (n - 1).leading_zeros();
    let half = bits.div_ceil(2);
    let mut value = x as u64;
    loop {
        value = feistel(value, half, seed);
        if value < n as u64 {
            return value as u32;
        }
    }
}

fn feistel(x: u64, half: u32, seed: u64) -> u64 {
    let mask = (1u64 << half) - 1;
    let mut left = (x >> half) & mask;
    let mut right = x & mask;
    for round in 0..4u64 {
        let f = splitmix64(seed ^ round.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ right) & mask;
        let next_left = right;
        let next_right = left ^ f;
        left = next_left;
        right = next_right;
    }
    (left << half) | right
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn frame_roundtrip() {
        let payload = b"hello watermark";
        let f = frame(payload).unwrap();
        assert_eq!(unframe(&f).unwrap(), payload);
        let mut bad = f.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(unframe(&bad).is_err());
    }

    #[test]
    fn permutation_is_bijective() {
        let n = 10_000u32;
        let seed = seed_from_key("test-key");
        let mut seen = HashSet::new();
        for x in 0..n {
            let p = permute(x, n, seed);
            assert!(p < n);
            assert!(seen.insert(p), "duplicate value {p}");
        }
        assert_eq!(seen.len(), n as usize);
    }

    #[test]
    fn embedding_roundtrip() {
        let payload = b"{\"secret\":true}";
        let frame = frame(payload).unwrap();
        let slots = needed_slots(frame.len()) + 37;
        let mut carrier = vec![0u8; slots];
        let seed = seed_from_key("k");
        embed_with(&frame, slots, seed, |i, bit| carrier[i] = bit).unwrap();
        let recovered = extract_with(slots, seed, |i| carrier[i]).unwrap();
        assert_eq!(recovered, payload);
    }
}
