//! Metadata layer: a small XMP packet carrying the signed record.
//!
//! Input files are fully re-encoded, so every pre-existing EXIF/GPS/IPTC/XMP
//! block is discarded. Only the freshly built block below is written.

use base64::Engine;

pub const XMP_APP1_PREFIX: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
pub const PNG_XMP_KEYWORD: &str = "XML:com.adobe.xmp";
const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

pub fn build_xmp(payload_json: &str, id: &str, utc: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload_json.as_bytes());
    let id = xml_escape(id);
    let utc = xml_escape(utc);
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"watermarker/0.1\">\n\
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
  <rdf:Description rdf:about=\"\" xmlns:wmp=\"https://watermarker.invalid/ns/1.0/\" \
xmlns:xmpMM=\"http://ns.adobe.com/xap/1.0/mm/\">\n\
   <wmp:Payload>{encoded}</wmp:Payload>\n\
   <wmp:Id>{id}</wmp:Id>\n\
   <wmp:CreatedUtc>{utc}</wmp:CreatedUtc>\n\
   <xmpMM:DocumentID>wm:{id}</xmpMM:DocumentID>\n\
  </rdf:Description>\n\
 </rdf:RDF>\n\
</x:xmpmeta>\n\
<?xpacket end=\"w\"?>"
    )
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Pull the base64 JSON record out of an XMP packet.
pub fn payload_from_xmp(xmp: &str) -> Option<String> {
    let open = "<wmp:Payload>";
    let close = "</wmp:Payload>";
    let start = xmp.find(open)? + open.len();
    let end = xmp[start..].find(close)? + start;
    let b64: String = xmp[start..end].chars().filter(|c| !c.is_whitespace()).collect();
    let raw = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    String::from_utf8(raw).ok()
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn png_crc(kind: &[u8], data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(kind);
    hasher.update(data);
    hasher.finalize()
}

/// Insert an iTXt chunk (standard XMP location) before IEND.
pub fn inject_png_xmp(png: &[u8], xmp: &str) -> Result<Vec<u8>, String> {
    if !png.starts_with(PNG_SIGNATURE) {
        return Err("not a PNG file".into());
    }
    let mut pos = 8usize;
    let mut iend_at = None;
    while pos + 12 <= png.len() {
        let len = be32(png, pos) as usize;
        let kind = &png[pos + 4..pos + 8];
        let end = pos
            .checked_add(12)
            .and_then(|v| v.checked_add(len))
            .ok_or("corrupt PNG chunk length")?;
        if end > png.len() {
            return Err("corrupt PNG chunk length".into());
        }
        if kind == b"IEND" {
            iend_at = Some(pos);
            break;
        }
        pos = end;
    }
    let iend_at = iend_at.ok_or("PNG has no IEND chunk")?;

    let mut data = Vec::with_capacity(64 + xmp.len());
    data.extend_from_slice(PNG_XMP_KEYWORD.as_bytes());
    data.push(0); // keyword terminator
    data.push(0); // compression flag
    data.push(0); // compression method
    data.push(0); // empty language tag
    data.push(0); // empty translated keyword
    data.extend_from_slice(xmp.as_bytes());

    let mut chunk = Vec::with_capacity(12 + data.len());
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"iTXt");
    chunk.extend_from_slice(&data);
    chunk.extend_from_slice(&png_crc(b"iTXt", &data).to_be_bytes());

    let mut out = Vec::with_capacity(png.len() + chunk.len());
    out.extend_from_slice(&png[..iend_at]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[iend_at..]);
    Ok(out)
}

pub fn read_png_xmp(png: &[u8]) -> Option<String> {
    if !png.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut pos = 8usize;
    while pos + 12 <= png.len() {
        let len = be32(png, pos) as usize;
        let kind = &png[pos + 4..pos + 8];
        let data = png.get(pos + 8..pos + 8 + len)?;
        if kind == b"iTXt" || kind == b"tEXt" {
            let keyword_end = data.iter().position(|&b| b == 0)?;
            let keyword = std::str::from_utf8(&data[..keyword_end]).ok()?;
            if keyword == PNG_XMP_KEYWORD {
                let text = if kind == b"iTXt" {
                    let rest = &data[keyword_end + 1..];
                    if rest.len() < 2 {
                        return None;
                    }
                    let mut cursor = 2usize;
                    // language tag
                    let lang_end = rest[cursor..].iter().position(|&b| b == 0)? + cursor;
                    cursor = lang_end + 1;
                    let trans_end = rest[cursor..].iter().position(|&b| b == 0)? + cursor;
                    cursor = trans_end + 1;
                    rest.get(cursor..)?
                } else {
                    &data[keyword_end + 1..]
                };
                return std::str::from_utf8(text).ok().map(str::to_owned);
            }
        }
        if kind == b"IEND" {
            break;
        }
        pos += 12 + len;
    }
    None
}

fn app1_segment(xmp: &str) -> Result<Vec<u8>, String> {
    let payload_len = XMP_APP1_PREFIX.len() + xmp.len();
    let seg_len = payload_len + 2;
    if seg_len > 0xffff {
        return Err("XMP packet is too large for a JPEG APP1 segment".into());
    }
    let mut seg = Vec::with_capacity(seg_len + 2);
    seg.extend_from_slice(&[0xff, 0xe1]);
    seg.extend_from_slice(&(seg_len as u16).to_be_bytes());
    seg.extend_from_slice(XMP_APP1_PREFIX);
    seg.extend_from_slice(xmp.as_bytes());
    Ok(seg)
}

fn comment_segment(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() + 2 > 0xffff {
        return None;
    }
    let mut seg = Vec::with_capacity(bytes.len() + 4);
    seg.extend_from_slice(&[0xff, 0xfe]);
    seg.extend_from_slice(&((bytes.len() + 2) as u16).to_be_bytes());
    seg.extend_from_slice(bytes);
    Some(seg)
}

/// Insert XMP (and a human-readable COM comment) after the JFIF APP0 segment.
pub fn inject_jpeg_xmp(jpeg: &[u8], xmp: &str, comment: &str) -> Result<Vec<u8>, String> {
    if jpeg.len() < 4 || jpeg[0] != 0xff || jpeg[1] != 0xd8 {
        return Err("not a JPEG file".into());
    }
    let mut at = 2usize;
    if jpeg.len() >= 6 && jpeg[2] == 0xff && jpeg[3] == 0xe0 {
        let seg_len = u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize;
        if 4 + seg_len <= jpeg.len() {
            at = 4 + seg_len;
        }
    }
    let mut out = Vec::with_capacity(jpeg.len() + xmp.len() + 128);
    out.extend_from_slice(&jpeg[..at]);
    if let Some(seg) = comment_segment(comment) {
        out.extend_from_slice(&seg);
    }
    out.extend_from_slice(&app1_segment(xmp)?);
    out.extend_from_slice(&jpeg[at..]);
    Ok(out)
}

pub fn read_jpeg_xmp(jpeg: &[u8]) -> Option<String> {
    if jpeg.len() < 4 || jpeg[0] != 0xff || jpeg[1] != 0xd8 {
        return None;
    }
    let mut pos = 2usize;
    while pos + 4 <= jpeg.len() {
        if jpeg[pos] != 0xff {
            return None;
        }
        let marker = jpeg[pos + 1];
        if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            pos += 2;
            continue;
        }
        if marker == 0xd9 {
            break;
        }
        let seg_len = u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]) as usize;
        if seg_len < 2 || pos + 2 + seg_len > jpeg.len() {
            return None;
        }
        let payload = &jpeg[pos + 4..pos + 2 + seg_len];
        if marker == 0xe1 && payload.starts_with(XMP_APP1_PREFIX) {
            let xmp = &payload[XMP_APP1_PREFIX.len()..];
            return std::str::from_utf8(xmp).ok().map(str::to_owned);
        }
        pos += 2 + seg_len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_roundtrip() {
        let json = r#"{"id":"abc","text":"CONFIDENTIAL"}"#;
        let xmp = build_xmp(json, "abc", "2026-01-01T00:00:00Z");
        assert_eq!(payload_from_xmp(&xmp).unwrap(), json);
    }
}
