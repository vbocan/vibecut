//! GoPro Metadata Format (GPMF) parser — pure Rust, no FFI.
//!
//! GPMF is a KLV-style binary format embedded as a `gpmd` track in GoPro MP4
//! files. It carries telemetry (accelerometer, GPS, gyro), HiLight tags, and
//! various per-frame metadata. Spec: <https://github.com/gopro/gpmf-parser>.
//!
//! # Layout
//!
//! Each entry is 8 header bytes + payload, padded to a 4-byte boundary:
//!
//! ```text
//! offset  size  field
//!   0      4    FourCC key (ASCII; e.g. "DEVC", "STRM", "ACCL")
//!   4      1    type byte (0 = nested GPMF; ASCII for typed scalars)
//!   5      1    struct_size — bytes per element
//!   6      2    repeat — element count, big-endian
//!   8     N*M   payload (struct_size * repeat bytes)
//!   ...    pad  zero-pad up to 4-byte boundary
//! ```
//!
//! All multi-byte integers in the format are big-endian.
//!
//! Nested entries (type=0) carry a stream of further KLV entries inside their
//! payload. Top-level GPMF data is a sequence of `DEVC` (device) entries,
//! each containing one or more `STRM` (stream) entries, each carrying one
//! kind of sample (`ACCL`, `GPS5`, `GYRO`, ...).

use std::convert::TryInto;

pub type FourCC = [u8; 4];

/// Decoded view of a GPMF entry. `payload` is the raw, unpadded bytes.
#[derive(Debug, Clone, Copy)]
pub struct Klv<'a> {
    pub key: FourCC,
    pub type_byte: u8,
    pub struct_size: u8,
    pub repeat: u16,
    pub payload: &'a [u8],
}

impl<'a> Klv<'a> {
    pub fn key_str(&self) -> &str {
        std::str::from_utf8(&self.key).unwrap_or("????")
    }

    /// Type-0 entries hold a nested GPMF stream.
    pub fn is_nested(&self) -> bool {
        self.type_byte == 0
    }

    /// Iterate the nested entries, if this is a nested KLV.
    pub fn iter_nested(&self) -> Option<KlvIter<'a>> {
        if self.is_nested() {
            Some(KlvIter::new(self.payload))
        } else {
            None
        }
    }

    #[allow(dead_code)] // helper for debugging GPMF tree dumps
    pub fn payload_size(&self) -> usize {
        self.struct_size as usize * self.repeat as usize
    }

    // -------- typed accessors --------

    /// ASCII string (`'c'`).
    pub fn as_str(&self) -> Option<&'a str> {
        if self.type_byte == b'c' {
            let raw = std::str::from_utf8(self.payload).ok()?;
            Some(raw.trim_end_matches('\0').trim())
        } else {
            None
        }
    }

    /// Array of `u32` (`'L'`).
    pub fn as_u32_array(&self) -> Option<Vec<u32>> {
        if self.type_byte == b'L' && self.struct_size == 4 {
            Some(read_be(self.payload, 4, |c| u32::from_be_bytes(c.try_into().unwrap())))
        } else {
            None
        }
    }

    /// Array of `i16` (`'s'`). Used by `ACCL`, `GYRO`.
    pub fn as_i16_array(&self) -> Option<Vec<i16>> {
        if self.type_byte == b's' && self.struct_size as usize % 2 == 0 {
            Some(read_be(self.payload, 2, |c| i16::from_be_bytes(c.try_into().unwrap())))
        } else {
            None
        }
    }

    /// Array of `f32` (`'f'`).
    pub fn as_f32_array(&self) -> Option<Vec<f32>> {
        if self.type_byte == b'f' && self.struct_size == 4 {
            Some(read_be(self.payload, 4, |c| f32::from_be_bytes(c.try_into().unwrap())))
        } else {
            None
        }
    }

    /// Array of `i32` (`'l'`).
    pub fn as_i32_array(&self) -> Option<Vec<i32>> {
        if self.type_byte == b'l' && self.struct_size == 4 {
            Some(read_be(self.payload, 4, |c| i32::from_be_bytes(c.try_into().unwrap())))
        } else {
            None
        }
    }

    /// Array of `u64` (`'J'`).
    pub fn as_u64_array(&self) -> Option<Vec<u64>> {
        if self.type_byte == b'J' && self.struct_size == 8 {
            Some(read_be(self.payload, 8, |c| u64::from_be_bytes(c.try_into().unwrap())))
        } else {
            None
        }
    }

    /// Array of `u16` (`'S'`).
    pub fn as_u16_array(&self) -> Option<Vec<u16>> {
        if self.type_byte == b'S' && self.struct_size == 2 {
            Some(read_be(self.payload, 2, |c| u16::from_be_bytes(c.try_into().unwrap())))
        } else {
            None
        }
    }
}

fn read_be<T>(payload: &[u8], elem_size: usize, decode: impl Fn(&[u8]) -> T) -> Vec<T> {
    payload
        .chunks_exact(elem_size)
        .map(|c| decode(c))
        .collect()
}

/// Iterate KLV entries from a byte slice.
pub struct KlvIter<'a> {
    remaining: &'a [u8],
}

impl<'a> KlvIter<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }
}

impl<'a> Iterator for KlvIter<'a> {
    type Item = Klv<'a>;
    fn next(&mut self) -> Option<Klv<'a>> {
        if self.remaining.len() < 8 {
            return None;
        }
        let key: FourCC = self.remaining[0..4].try_into().unwrap();
        let type_byte = self.remaining[4];
        let struct_size = self.remaining[5];
        let repeat = u16::from_be_bytes([self.remaining[6], self.remaining[7]]);
        let payload_size = struct_size as usize * repeat as usize;
        let padded = (payload_size + 3) & !3;
        if 8 + padded > self.remaining.len() {
            // Malformed — stop iterating rather than panic.
            return None;
        }
        let payload = &self.remaining[8..8 + payload_size];
        let klv = Klv {
            key,
            type_byte,
            struct_size,
            repeat,
            payload,
        };
        self.remaining = &self.remaining[8 + padded..];
        Some(klv)
    }
}

// ---------- Tree walk for debug ----------

/// Walk the tree, calling `visit` for every entry with its current depth.
/// Stops gracefully on truncated/malformed payloads.
#[allow(dead_code)] // exposed for downstream tools / tests
pub fn walk<F: FnMut(usize, &Klv<'_>)>(bytes: &[u8], mut visit: F) {
    fn walk_rec<F: FnMut(usize, &Klv<'_>)>(bytes: &[u8], depth: usize, visit: &mut F) {
        for klv in KlvIter::new(bytes) {
            visit(depth, &klv);
            if klv.is_nested() {
                walk_rec(klv.payload, depth + 1, visit);
            }
        }
    }
    walk_rec(bytes, 0, &mut visit);
}

/// Format the tree as a human-readable string for inspection. Pass
/// `max_payloads` to limit how many top-level DEVC entries to descend into
/// (use `usize::MAX` for the whole stream).
pub fn format_tree(bytes: &[u8], max_payloads: usize) -> String {
    let mut out = String::new();
    let mut top_level_devc = 0usize;
    let mut suppressed = false;

    for klv in KlvIter::new(bytes) {
        if &klv.key == b"DEVC" {
            top_level_devc += 1;
            if top_level_devc > max_payloads {
                if !suppressed {
                    out.push_str(&format!(
                        "... (suppressed remaining {} top-level DEVC payloads)\n",
                        "?"
                    ));
                    suppressed = true;
                }
                continue;
            }
        }
        format_one(&klv, 0, &mut out);
    }
    out
}

fn format_one(klv: &Klv<'_>, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let typ = if klv.type_byte == 0 {
        "nested".to_string()
    } else {
        let c = klv.type_byte as char;
        format!("'{}'", if c.is_ascii_graphic() { c } else { '.' })
    };
    let mut sample = String::new();
    if klv.type_byte != 0 {
        sample = sample_string(klv);
    }
    out.push_str(&format!(
        "{indent}{key}  type={typ:<7}  {sz}x{rep}  {sample}\n",
        key = klv.key_str(),
        sz = klv.struct_size,
        rep = klv.repeat,
    ));
    if let Some(it) = klv.iter_nested() {
        for child in it {
            format_one(&child, depth + 1, out);
        }
    }
}

fn sample_string(klv: &Klv<'_>) -> String {
    if let Some(s) = klv.as_str() {
        return format!("\"{}\"", s.chars().take(64).collect::<String>());
    }
    if let Some(v) = klv.as_u32_array() {
        return format!("u32{:?}", &v[..v.len().min(6)]);
    }
    if let Some(v) = klv.as_i32_array() {
        return format!("i32{:?}", &v[..v.len().min(6)]);
    }
    if let Some(v) = klv.as_f32_array() {
        return format!("f32{:?}", &v[..v.len().min(6)]);
    }
    if let Some(v) = klv.as_i16_array() {
        return format!("i16{:?}", &v[..v.len().min(8)]);
    }
    if let Some(v) = klv.as_u16_array() {
        return format!("u16{:?}", &v[..v.len().min(8)]);
    }
    if let Some(v) = klv.as_u64_array() {
        return format!("u64{:?}", &v[..v.len().min(4)]);
    }
    format!("({} bytes)", klv.payload.len())
}

// ---------- High-level extractors (filled in after observing real data) ----------

/// One top-level GPMF "payload" — typically one DEVC corresponding to ~1s of
/// video. Carries an ordered list of streams.
#[derive(Debug, Clone)]
pub struct PayloadView<'a> {
    pub devc: Klv<'a>,
}

/// Iterate top-level DEVC payloads.
pub fn iter_payloads(bytes: &[u8]) -> impl Iterator<Item = PayloadView<'_>> {
    KlvIter::new(bytes)
        .filter(|k| &k.key == b"DEVC" && k.is_nested())
        .map(|k| PayloadView { devc: k })
}

// ---------- High-level: per-payload telemetry ----------

/// One stream's samples extracted from a single DEVC payload.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `key`/`name` are part of the public stream identifier surface
pub struct StreamSamples<'a> {
    pub key: FourCC,
    pub name: Option<&'a str>,
    pub raw: Klv<'a>,
    pub scale: Option<Vec<f32>>,
}

/// Walk a DEVC payload and return the inner STRM whose data key matches
/// `target` (e.g. `b"ACCL"`). Returns the data KLV plus its `SCAL` (if any)
/// and `STNM` (if any).
pub fn find_stream_in_payload<'a>(
    devc: Klv<'a>,
    target: &FourCC,
) -> Option<StreamSamples<'a>> {
    let strms = devc.iter_nested()?;
    for strm in strms.filter(|k| &k.key == b"STRM" && k.is_nested()) {
        let mut name: Option<&str> = None;
        let mut scale: Option<Vec<f32>> = None;
        let mut data: Option<Klv<'a>> = None;
        for child in strm.iter_nested().unwrap() {
            match &child.key {
                b"STNM" => name = child.as_str(),
                b"SCAL" => scale = decode_scal(&child),
                k if k == target => data = Some(child),
                _ => {}
            }
        }
        if let Some(d) = data {
            return Some(StreamSamples {
                key: d.key,
                name,
                raw: d,
                scale,
            });
        }
    }
    None
}

fn decode_scal(klv: &Klv<'_>) -> Option<Vec<f32>> {
    // SCAL values in HERO13 GPMF are typically int16. Some firmware uses int32.
    if let Some(v) = klv.as_i16_array() {
        return Some(v.into_iter().map(|x| x as f32).collect());
    }
    if let Some(v) = klv.as_i32_array() {
        return Some(v.into_iter().map(|x| x as f32).collect());
    }
    if let Some(v) = klv.as_f32_array() {
        return Some(v);
    }
    None
}

/// Time-stamped accelerometer magnitude samples extracted from a GPMF stream.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `seconds_per_payload` is part of the public timing surface
pub struct AccelSeries {
    /// Magnitude `||a||` in m/s², one entry per accelerometer sample,
    /// in time order across the whole file.
    pub magnitude: Vec<f32>,
    /// Number of samples in each per-payload block (for time inference).
    /// `payload_lens[i]` samples cover the i-th DEVC payload's window.
    pub payload_lens: Vec<u32>,
    /// Total file duration spanned by the GPMF stream, derived from payload count
    /// (each payload is treated as ~1 video-frame-window — see `seconds_per_payload`).
    pub seconds_per_payload: f32,
}

#[allow(dead_code)] // public time-base helpers, kept for downstream tools
impl AccelSeries {
    /// Wall-clock time of the i-th magnitude sample, in seconds since file start.
    pub fn time_of(&self, i: usize) -> f32 {
        // Walk payload_lens to find which payload `i` belongs to and where.
        let mut acc = 0usize;
        for (p, &len) in self.payload_lens.iter().enumerate() {
            let n = len as usize;
            if i < acc + n {
                let frac = (i - acc) as f32 / n.max(1) as f32;
                return (p as f32 + frac) * self.seconds_per_payload;
            }
            acc += n;
        }
        self.payload_lens.len() as f32 * self.seconds_per_payload
    }

    pub fn total_samples(&self) -> usize {
        self.magnitude.len()
    }
}

/// Extract the accelerometer magnitude time-series from raw GPMF bytes.
///
/// Each DEVC payload contributes ~one second of samples (default
/// `seconds_per_payload = 1.0` — refined by callers who know the camera's
/// payload rate from elsewhere).
pub fn extract_accel(bytes: &[u8]) -> AccelSeries {
    let mut magnitude = Vec::new();
    let mut payload_lens = Vec::new();

    for payload in iter_payloads(bytes) {
        let Some(stream) = find_stream_in_payload(payload.devc, b"ACCL") else {
            payload_lens.push(0);
            continue;
        };
        let scale = stream
            .scale
            .as_ref()
            .and_then(|v| v.first().copied())
            .unwrap_or(1.0);
        let raw = stream.raw.as_i16_array().unwrap_or_default();
        // ACCL is 3-axis (x, y, z) interleaved; struct_size should be 6 bytes.
        let mut count_in_payload = 0u32;
        for triplet in raw.chunks_exact(3) {
            let x = triplet[0] as f32 / scale;
            let y = triplet[1] as f32 / scale;
            let z = triplet[2] as f32 / scale;
            magnitude.push((x * x + y * y + z * z).sqrt());
            count_in_payload += 1;
        }
        payload_lens.push(count_in_payload);
    }

    AccelSeries {
        magnitude,
        payload_lens,
        seconds_per_payload: 1.0,
    }
}

/// Try to locate a "Highlights" stream within the GPMF data and return the
/// HiLight timestamps in milliseconds since file start. On HERO13 / modern
/// cameras these are usually empty unless the user pressed the tag button.
pub fn extract_hilights_from_gpmf(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    for payload in iter_payloads(bytes) {
        let Some(strms) = payload.devc.iter_nested() else {
            continue;
        };
        for strm in strms.filter(|k| &k.key == b"STRM" && k.is_nested()) {
            let mut is_highlights = false;
            let mut data: Option<Klv<'_>> = None;
            for child in strm.iter_nested().unwrap() {
                match &child.key {
                    b"STNM" => {
                        if let Some(name) = child.as_str() {
                            let lower = name.to_lowercase();
                            if lower.contains("highlight")
                                || lower.contains("hilight")
                                || lower.contains("hilite")
                            {
                                is_highlights = true;
                            }
                        }
                    }
                    b"HLMT" | b"HMMT" | b"MARK" => data = Some(child),
                    _ => {}
                }
            }
            if is_highlights {
                if let Some(d) = data {
                    if let Some(v) = d.as_u32_array() {
                        out.extend(v);
                    }
                }
            }
        }
    }
    out
}

// ---------- MP4 udta HMMT atom (legacy HiLight format) ----------

/// Parse the HiLight tag table from a chunk of MP4 bytes that contains the
/// `udta` atom (or is the entire MP4 file). Returns timestamps in milliseconds.
/// Returns `Vec::new()` if no HMMT atom is found or it is empty.
///
/// Format: 4-byte BE atom size, then `HMMT`, then 4-byte BE count, then
/// `count` × 4-byte BE timestamp.
pub fn parse_hmmt_from_mp4(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let needle = b"HMMT";
    let mut search_from = 0usize;
    while let Some(rel) = find_subslice(&bytes[search_from..], needle) {
        let i = search_from + rel;
        // Need 4 bytes before for atom size, then HMMT, then 4-byte count,
        // then count*4 bytes of timestamps.
        if i < 4 {
            search_from = i + 4;
            continue;
        }
        let atom_size = u32::from_be_bytes(bytes[i - 4..i].try_into().unwrap()) as usize;
        if atom_size < 12 || i - 4 + atom_size > bytes.len() {
            search_from = i + 4;
            continue;
        }
        let body_start = i + 4;
        let body_end = i - 4 + atom_size;
        let body = &bytes[body_start..body_end];
        if body.len() < 4 {
            search_from = i + 4;
            continue;
        }
        let count = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
        for j in 0..count {
            let off = 4 + j * 4;
            if off + 4 > body.len() {
                break;
            }
            out.push(u32::from_be_bytes(body[off..off + 4].try_into().unwrap()));
        }
        if !out.is_empty() {
            return out;
        }
        search_from = i + 4;
    }
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one KLV blob: header + payload + zero-padding to 4 bytes.
    fn klv(key: &[u8; 4], type_byte: u8, struct_size: u8, repeat: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(key);
        v.push(type_byte);
        v.push(struct_size);
        v.extend_from_slice(&repeat.to_be_bytes());
        v.extend_from_slice(payload);
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    }

    #[test]
    fn parses_simple_klv() {
        let payload: [u8; 4] = 42u32.to_be_bytes();
        let bytes = klv(b"DVID", b'L', 4, 1, &payload);
        let parsed: Vec<_> = KlvIter::new(&bytes).collect();
        assert_eq!(parsed.len(), 1);
        let k = parsed[0];
        assert_eq!(&k.key, b"DVID");
        assert_eq!(k.type_byte, b'L');
        assert_eq!(k.as_u32_array().unwrap(), vec![42]);
    }

    #[test]
    fn parses_nested() {
        // STRM { STNM "ACCL" }
        let stnm_payload = b"ACCL\0\0\0\0"; // 4 chars + pad would normally be (size,repeat)=(1,4)
        let stnm = klv(b"STNM", b'c', 1, 4, b"ACCL");
        let strm = klv(b"STRM", 0, 1, stnm.len() as u16, &stnm);
        let _ = stnm_payload; // silence
        let parsed: Vec<_> = KlvIter::new(&strm).collect();
        assert_eq!(parsed.len(), 1);
        let outer = parsed[0];
        assert!(outer.is_nested());
        let inner: Vec<_> = outer.iter_nested().unwrap().collect();
        assert_eq!(inner.len(), 1);
        assert_eq!(&inner[0].key, b"STNM");
        assert_eq!(inner[0].as_str().unwrap(), "ACCL");
    }

    #[test]
    fn handles_padding() {
        // 3-byte payload should pad to 4.
        let body = klv(b"TEST", b'B', 1, 3, &[1, 2, 3]);
        assert_eq!(body.len(), 12); // 8 header + 4 padded payload
        let parsed: Vec<_> = KlvIter::new(&body).collect();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].payload, &[1, 2, 3]);
    }

    #[test]
    fn truncated_input_does_not_panic() {
        // Header claims 100 bytes payload but we only give 5.
        let mut v = Vec::new();
        v.extend_from_slice(b"BIG!");
        v.push(b'B');
        v.push(1);
        v.extend_from_slice(&100u16.to_be_bytes());
        v.extend_from_slice(&[1, 2, 3, 4, 5]);
        let parsed: Vec<_> = KlvIter::new(&v).collect();
        assert!(parsed.is_empty()); // truncated -> stops cleanly
    }

    #[test]
    fn i16_array_decodes_be() {
        let body = klv(b"ACCL", b's', 6, 1, &[0, 1, 0, 2, 0, 3]);
        let parsed: Vec<_> = KlvIter::new(&body).collect();
        let arr = parsed[0].as_i16_array().unwrap();
        assert_eq!(arr, vec![1, 2, 3]);
    }
}
