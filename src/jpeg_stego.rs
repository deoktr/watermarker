//! LSB steganography in quantised JPEG DCT coefficients.
//!
//! The JPEG produced by our encoder is baseline sequential, so we decode its
//! entropy-coded scan, tweak the least significant bit of the magnitude of
//! usable AC coefficients, and re-encode the scan with the same Huffman tables.
//! Fully invisible; any re-compression or resize destroys the payload.

use std::collections::HashMap;

use crate::stego;

pub struct EmbedInfo {
    pub bytes_out: Vec<u8>,
    pub slots: usize,
    pub required: usize,
}

#[derive(Clone, Default)]
struct Huff {
    min_code: [i32; 17],
    max_code: [i32; 17],
    val_ptr: [u32; 17],
    vals: Vec<u8>,
    enc: Vec<Option<(u16, u8)>>,
}

impl Huff {
    fn new(bits: [u8; 16], vals: Vec<u8>) -> Self {
        let mut h = Huff::default();

        let mut code = 0i32;
        let mut consumed = 0u32;
        for len in 1..=16 {
            let count = bits[len - 1] as i32;
            h.val_ptr[len] = consumed;
            h.min_code[len] = code;
            code += count;
            consumed += count as u32;
            h.max_code[len] = code - 1;
            code <<= 1;
        }
        h.vals = vals;

        let mut enc = vec![None; 256];
        let mut code = 0u32;
        let mut index = 0usize;
        for len in 1..=16 {
            for _ in 0..bits[len - 1] {
                if let Some(&symbol) = h.vals.get(index) {
                    enc[symbol as usize] = Some((code as u16, len as u8));
                }
                index += 1;
                code += 1;
            }
            code <<= 1;
        }
        h.enc = enc;
        h
    }

    fn decode(&self, reader: &mut BitReader) -> Result<u8, String> {
        let mut code = 0i32;
        for len in 1..=16 {
            code = (code << 1) | reader.read_bit()? as i32;
            let max = self.max_code[len];
            if max >= self.min_code[len] && code <= max {
                let index = self.val_ptr[len] as i32 + code - self.min_code[len];
                return self
                    .vals
                    .get(index as usize)
                    .copied()
                    .ok_or_else(|| "corrupt JPEG Huffman symbol".to_string());
            }
        }
        Err("invalid JPEG Huffman code in scan".into())
    }

    fn encode_symbol(&self, symbol: u8) -> Result<(u16, u8), String> {
        self.enc
            .get(symbol as usize)
            .copied()
            .flatten()
            .ok_or_else(|| format!("JPEG Huffman table has no code for symbol 0x{symbol:02x}"))
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    current: u32,
    remaining: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            current: 0,
            remaining: 0,
        }
    }

    fn read_bit(&mut self) -> Result<u32, String> {
        if self.remaining == 0 {
            let byte = *self
                .data
                .get(self.pos)
                .ok_or_else(|| "unexpected end of JPEG scan data".to_string())?;
            self.pos += 1;
            self.current = byte as u32;
            self.remaining = 8;
        }
        self.remaining -= 1;
        Ok((self.current >> self.remaining) & 1)
    }

    fn read_bits(&mut self, count: u8) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | self.read_bit()?;
        }
        Ok(value)
    }
}

struct BitWriter {
    out: Vec<u8>,
    current: u8,
    used: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            current: 0,
            used: 0,
        }
    }

    fn write(&mut self, value: u32, count: u8) {
        for shift in (0..count).rev() {
            self.push(((value >> shift) & 1) as u8);
        }
    }

    fn push(&mut self, bit: u8) {
        self.current = (self.current << 1) | (bit & 1);
        self.used += 1;
        if self.used == 8 {
            self.emit(self.current);
            self.current = 0;
            self.used = 0;
        }
    }

    fn emit(&mut self, byte: u8) {
        self.out.push(byte);
        if byte == 0xff {
            self.out.push(0x00);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            let byte = self.current << (8 - self.used);
            self.emit(byte);
        }
        self.out
    }
}

#[derive(Clone)]
struct Component {
    h: usize,
    v: usize,
    dc: Huff,
    ac: Huff,
}

struct JpegImage {
    header: Vec<u8>,
    components: Vec<Component>,
    blocks_x: Vec<usize>,
    mcus_x: usize,
    mcus_y: usize,
    coeffs: Vec<Vec<[i32; 64]>>,
}

fn be16(data: &[u8], at: usize) -> Result<u16, String> {
    if at + 2 > data.len() {
        return Err("truncated JPEG marker".into());
    }
    Ok(u16::from_be_bytes([data[at], data[at + 1]]))
}

fn unstuff(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    while i < raw.len() {
        let byte = raw[i];
        if byte == 0xff {
            match raw.get(i + 1) {
                Some(0x00) => {
                    out.push(0xff);
                    i += 2;
                }
                Some(&marker) if (0xd0..=0xd7).contains(&marker) => {
                    return Err("JPEG restart markers are not supported".into());
                }
                _ => return Err("unexpected marker inside JPEG scan".into()),
            }
        } else {
            out.push(byte);
            i += 1;
        }
    }
    Ok(out)
}

fn extend(value: u32, size: u8) -> i32 {
    let value = value as i32;
    let threshold = 1i32 << (size - 1);
    if value < threshold {
        value - ((1i32 << size) - 1)
    } else {
        value
    }
}

fn category(value: i32) -> u8 {
    if value == 0 {
        0
    } else {
        32 - value.unsigned_abs().leading_zeros() as u8
    }
}

fn amplitude(value: i32, size: u8) -> u32 {
    if value >= 0 {
        value as u32
    } else {
        (value + ((1i32 << size) - 1)) as u32
    }
}

fn decode_block(
    reader: &mut BitReader,
    dc: &Huff,
    ac: &Huff,
    predictor: &mut i32,
) -> Result<[i32; 64], String> {
    let mut block = [0i32; 64];
    let dc_size = dc.decode(reader)?;
    let diff = if dc_size == 0 {
        0
    } else {
        extend(reader.read_bits(dc_size)?, dc_size)
    };
    *predictor = predictor.wrapping_add(diff);
    block[0] = *predictor;

    let mut k = 1usize;
    while k < 64 {
        let symbol = ac.decode(reader)?;
        let size = symbol & 0x0f;
        let run = (symbol >> 4) as usize;
        if size == 0 {
            if run == 15 {
                k += 16;
                continue;
            }
            break;
        }
        k += run;
        if k >= 64 {
            return Err("corrupt JPEG AC run length".into());
        }
        block[k] = extend(reader.read_bits(size)?, size);
        k += 1;
    }
    Ok(block)
}

fn encode_block(
    writer: &mut BitWriter,
    block: &[i32; 64],
    dc: &Huff,
    ac: &Huff,
    predictor: &mut i32,
) -> Result<(), String> {
    let diff = block[0].wrapping_sub(*predictor);
    *predictor = block[0];
    let dc_size = category(diff);
    let (code, len) = dc.encode_symbol(dc_size)?;
    writer.write(code as u32, len);
    if dc_size > 0 {
        writer.write(amplitude(diff, dc_size), dc_size);
    }

    let mut run = 0usize;
    for k in 1..64 {
        let value = block[k];
        if value == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            let (code, len) = ac.encode_symbol(0xf0)?;
            writer.write(code as u32, len);
            run -= 16;
        }
        let size = category(value);
        let symbol = ((run as u8) << 4) | size;
        let (code, len) = ac.encode_symbol(symbol)?;
        writer.write(code as u32, len);
        writer.write(amplitude(value, size), size);
        run = 0;
    }
    if run > 0 {
        let (code, len) = ac.encode_symbol(0x00)?;
        writer.write(code as u32, len);
    }
    Ok(())
}

impl JpegImage {
    fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
            return Err("not a baseline JPEG file".into());
        }

        let mut pos = 2usize;
        let mut width = 0usize;
        let mut height = 0usize;
        let mut description: Vec<(u8, usize, usize)> = Vec::new();
        let mut huff_tables: HashMap<(u8, u8), Huff> = HashMap::new();
        let mut selectors: Vec<(u8, u8, u8)> = Vec::new();
        let mut scan_start = None;
        let mut scan_end = None;
        let mut saw_scan = false;
        let mut blocks_x = Vec::new();

        while pos < data.len() {
            if data[pos] != 0xff {
                return Err("malformed JPEG marker stream".into());
            }
            while pos < data.len() && data[pos] == 0xff {
                pos += 1;
            }
            if pos >= data.len() {
                break;
            }
            let marker = data[pos];
            pos += 1;

            match marker {
                0xd8 | 0x01 => continue,
                0xd9 => break,
                0xd0..=0xd7 => continue,
                _ => {}
            }

            let length = be16(data, pos)? as usize;
            if length < 2 || pos + length > data.len() {
                return Err("corrupt JPEG segment length".into());
            }
            let payload = &data[pos + 2..pos + length];

            match marker {
                0xc0 | 0xc1 => {
                    if payload.len() < 6 {
                        return Err("corrupt JPEG SOF segment".into());
                    }
                    if payload[0] != 8 {
                        return Err("only 8-bit JPEG is supported".into());
                    }
                    height = u16::from_be_bytes([payload[1], payload[2]]) as usize;
                    width = u16::from_be_bytes([payload[3], payload[4]]) as usize;
                    let count = payload[5] as usize;
                    if count == 0 || count > 4 || payload.len() < 6 + count * 3 {
                        return Err("corrupt JPEG component table".into());
                    }
                    description.clear();
                    for index in 0..count {
                        let id = payload[6 + index * 3];
                        let hv = payload[7 + index * 3];
                        let h = (hv >> 4) as usize;
                        let v = (hv & 0x0f) as usize;
                        if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
                            return Err("invalid JPEG sampling factor".into());
                        }
                        description.push((id, h, v));
                    }
                }
                0xc2 | 0xc6 | 0xca | 0xce => {
                    return Err("progressive/arithmetic JPEG is not supported for DCT stego; save as PNG or baseline JPEG".into());
                }
                0xc4 => {
                    let mut cursor = 0usize;
                    while cursor < payload.len() {
                        if cursor + 17 > payload.len() {
                            return Err("corrupt JPEG DHT segment".into());
                        }
                        let class = payload[cursor] >> 4;
                        let id = payload[cursor] & 0x0f;
                        cursor += 1;
                        let mut bits = [0u8; 16];
                        bits.copy_from_slice(&payload[cursor..cursor + 16]);
                        cursor += 16;
                        let total: usize = bits.iter().map(|&b| b as usize).sum();
                        if cursor + total > payload.len() {
                            return Err("corrupt JPEG DHT symbol table".into());
                        }
                        let values = payload[cursor..cursor + total].to_vec();
                        cursor += total;
                        huff_tables.insert((class, id), Huff::new(bits, values));
                    }
                }
                0xdd => {
                    if payload.len() >= 2 {
                        let interval = u16::from_be_bytes([payload[0], payload[1]]);
                        if interval != 0 {
                            return Err("JPEG restart intervals are not supported".into());
                        }
                    }
                }
                0xda => {
                    if saw_scan {
                        return Err("multi-scan JPEG is not supported".into());
                    }
                    saw_scan = true;
                    if payload.is_empty() {
                        return Err("corrupt JPEG SOS segment".into());
                    }
                    let count = payload[0] as usize;
                    if payload.len() < 1 + count * 2 + 3 {
                        return Err("corrupt JPEG SOS component table".into());
                    }
                    selectors.clear();
                    for index in 0..count {
                        let id = payload[1 + index * 2];
                        let tables = payload[2 + index * 2];
                        selectors.push((id, tables >> 4, tables & 0x0f));
                    }
                    let ss = payload[1 + count * 2];
                    let se = payload[2 + count * 2];
                    let approximation = payload[3 + count * 2];
                    if ss != 0 || se != 63 || approximation != 0 {
                        return Err("progressive JPEG scans are not supported".into());
                    }

                    let header_end = pos + length;
                    scan_start = Some(header_end);
                    let mut cursor = header_end;
                    let mut end = None;
                    while cursor + 1 < data.len() {
                        if data[cursor] == 0xff {
                            let next = data[cursor + 1];
                            if next == 0x00 {
                                cursor += 2;
                                continue;
                            }
                            if (0xd0..=0xd7).contains(&next) {
                                return Err("JPEG restart markers are not supported".into());
                            }
                            if next == 0xff {
                                cursor += 1;
                                continue;
                            }
                            end = Some(cursor);
                            break;
                        }
                        cursor += 1;
                    }
                    let end =
                        end.ok_or_else(|| "JPEG scan has no terminating marker".to_string())?;
                    scan_end = Some(end);
                    pos = end;
                    continue;
                }
                _ => {}
            }

            pos += length;
        }

        let scan_start = scan_start.ok_or_else(|| "JPEG has no scan".to_string())?;
        let scan_end = scan_end.ok_or_else(|| "JPEG scan is truncated".to_string())?;
        if description.is_empty() || width == 0 || height == 0 {
            return Err("JPEG has no frame header".into());
        }
        if selectors.len() != description.len() {
            return Err("JPEG scan does not match frame components".into());
        }

        let max_h = description.iter().map(|c| c.1).max().unwrap_or(1);
        let max_v = description.iter().map(|c| c.2).max().unwrap_or(1);
        let mcus_x = (width + 8 * max_h - 1) / (8 * max_h);
        let mcus_y = (height + 8 * max_v - 1) / (8 * max_v);

        let mut components = Vec::with_capacity(description.len());
        for (id, h, v) in &description {
            let selector = selectors
                .iter()
                .find(|(component_id, _, _)| component_id == id)
                .ok_or_else(|| "JPEG scan references an unknown component".to_string())?;
            let dc = huff_tables
                .get(&(0, selector.1))
                .cloned()
                .ok_or_else(|| "JPEG is missing a DC Huffman table".to_string())?;
            let ac = huff_tables
                .get(&(1, selector.2))
                .cloned()
                .ok_or_else(|| "JPEG is missing an AC Huffman table".to_string())?;
            components.push(Component {
                h: *h,
                v: *v,
                dc,
                ac,
            });
            blocks_x.push(mcus_x * h);
        }

        let entropy = unstuff(&data[scan_start..scan_end])?;
        let mut reader = BitReader::new(&entropy);
        let mut predictors = vec![0i32; components.len()];
        let mut coeffs: Vec<Vec<[i32; 64]>> = components
            .iter()
            .map(|c| vec![[0i32; 64]; mcus_x * c.h * mcus_y * c.v])
            .collect();

        for mcu_y in 0..mcus_y {
            for mcu_x in 0..mcus_x {
                for (index, component) in components.iter().enumerate() {
                    for block_y in 0..component.v {
                        for block_x in 0..component.h {
                            let block = decode_block(
                                &mut reader,
                                &component.dc,
                                &component.ac,
                                &mut predictors[index],
                            )?;
                            let row = mcu_y * component.v + block_y;
                            let column = mcu_x * component.h + block_x;
                            let block_index = row * blocks_x[index] + column;
                            coeffs[index][block_index] = block;
                        }
                    }
                }
            }
        }

        Ok(JpegImage {
            header: data[..scan_start].to_vec(),
            components,
            blocks_x,
            mcus_x,
            mcus_y,
            coeffs,
        })
    }

    fn usable_positions(&self) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        for mcu_y in 0..self.mcus_y {
            for mcu_x in 0..self.mcus_x {
                for (index, component) in self.components.iter().enumerate() {
                    for block_y in 0..component.v {
                        for block_x in 0..component.h {
                            let row = mcu_y * component.v + block_y;
                            let column = mcu_x * component.h + block_x;
                            let block_index = row * self.blocks_x[index] + column;
                            let block = &self.coeffs[index][block_index];
                            for k in 1..64 {
                                if block[k].unsigned_abs() >= 2 {
                                    out.push((index, block_index, k));
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }

    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        let mut out = self.header.clone();
        let mut writer = BitWriter::new();
        let mut predictors = vec![0i32; self.components.len()];

        for mcu_y in 0..self.mcus_y {
            for mcu_x in 0..self.mcus_x {
                for (index, component) in self.components.iter().enumerate() {
                    for block_y in 0..component.v {
                        for block_x in 0..component.h {
                            let row = mcu_y * component.v + block_y;
                            let column = mcu_x * component.h + block_x;
                            let block_index = row * self.blocks_x[index] + column;
                            encode_block(
                                &mut writer,
                                &self.coeffs[index][block_index],
                                &component.dc,
                                &component.ac,
                                &mut predictors[index],
                            )?;
                        }
                    }
                }
            }
        }

        out.extend_from_slice(&writer.finish());
        out.extend_from_slice(&[0xff, 0xd9]);
        Ok(out)
    }
}

pub fn embed(jpeg: &[u8], frame: &[u8], seed: u64) -> Result<EmbedInfo, String> {
    let mut image = JpegImage::parse(jpeg)?;
    let positions = image.usable_positions();
    let slots = positions.len();
    let required = stego::needed_slots(frame.len());
    stego::embed_with(frame, slots, seed, |slot, bit| {
        let (component, block_index, k) = positions[slot];
        let coefficient = &mut image.coeffs[component][block_index][k];
        let negative = *coefficient < 0;
        let magnitude = (coefficient.unsigned_abs() & !1) | bit as u32;
        *coefficient = if negative {
            -(magnitude as i32)
        } else {
            magnitude as i32
        };
    })?;
    Ok(EmbedInfo {
        bytes_out: image.to_bytes()?,
        slots,
        required,
    })
}

pub fn extract(jpeg: &[u8], seed: u64) -> Result<Vec<u8>, String> {
    let image = JpegImage::parse(jpeg)?;
    let positions = image.usable_positions();
    stego::extract_with(positions.len(), seed, |slot| {
        let (component, block_index, k) = positions[slot];
        (image.coeffs[component][block_index][k].unsigned_abs() & 1) as u8
    })
}

/// Number of carrier coefficients available in this JPEG.
pub fn capacity(jpeg: &[u8]) -> Result<usize, String> {
    Ok(JpegImage::parse(jpeg)?.usable_positions().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huffman_standard_luma_dc() {
        // Standard JPEG Annex K Table K.3.
        let bits = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
        let vals = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let table = Huff::new(bits, vals);
        assert_eq!(table.encode_symbol(0).unwrap(), (0b00, 2));
        assert_eq!(table.encode_symbol(1).unwrap(), (0b010, 3));
        assert_eq!(table.encode_symbol(11).unwrap(), (0b111111110, 9));
    }

    #[test]
    fn dct_embed_roundtrip_and_still_decodes() {
        use jpeg_encoder::{ColorType, Encoder, SamplingFactor};

        let (width, height) = (96u16, 80u16);
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height as u32 {
            for x in 0..width as u32 {
                let noise = ((x * 7 + y * 13) % 23) as u8;
                rgb.push((x as u8).wrapping_mul(2).wrapping_add(noise));
                rgb.push((y as u8).wrapping_mul(3).wrapping_add(noise / 2));
                rgb.push(((x + y) as u8).wrapping_add(noise));
            }
        }
        let mut jpeg = Vec::new();
        {
            let mut encoder = Encoder::new(&mut jpeg, 90);
            encoder.set_sampling_factor(SamplingFactor::F_1_1);
            encoder.encode(&rgb, width, height, ColorType::Rgb).unwrap();
        }

        let frame = stego::frame(b"jpeg-secret").unwrap();
        let info = embed(&jpeg, &frame, 42).unwrap();
        assert!(info.slots > 0);
        assert_eq!(extract(&info.bytes_out, 42).unwrap(), b"jpeg-secret");
        assert!(extract(&info.bytes_out, 43).is_err());

        // The modified entropy stream must remain a valid JPEG.
        image::load_from_memory_with_format(&info.bytes_out, image::ImageFormat::Jpeg).unwrap();
    }
}
