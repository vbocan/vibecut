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
    /// Each payload's sample count and `STMP` / `TSMP` stamps (one entry per
    /// DEVC payload, parallel to `payload_lens`) — see [`sample_times`].
    pub timing: Vec<PayloadTiming>,
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

    /// Each magnitude sample's time in seconds from the start of a file
    /// `file_duration_secs` long, from the stream's own stamps where it
    /// has them ([`sample_times`]).
    pub fn times_secs(&self, file_duration_secs: f32) -> Vec<f32> {
        let mut t = sample_times(&self.timing, file_duration_secs);
        t.truncate(self.magnitude.len());
        t
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
    let mut timing = Vec::new();

    for payload in iter_payloads(bytes) {
        let Some(stream) = stream_payload(payload.devc, b"ACCL") else {
            payload_lens.push(0);
            timing.push(PayloadTiming::default());
            continue;
        };
        let scale = stream
            .scal
            .first()
            .map(|&s| s as f32)
            .filter(|&s| s != 0.0)
            .unwrap_or(1.0);
        let raw = stream.data.as_i16_array().unwrap_or_default();
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
        timing.push(PayloadTiming {
            samples: count_in_payload,
            stmp_us: stream.stmp_us,
            tsmp: stream.tsmp,
        });
    }

    AccelSeries {
        magnitude,
        payload_lens,
        seconds_per_payload: 1.0,
        timing,
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

// ---------- Generic stream decoding ----------

/// Bytes per value of a GPMF scalar type, `None` for nested / complex /
/// unknown types.
fn scalar_size(t: u8) -> Option<usize> {
    Some(match t {
        b'b' | b'B' | b'c' => 1,
        b's' | b'S' => 2,
        b'l' | b'L' | b'f' | b'q' => 4,
        b'd' | b'j' | b'J' | b'Q' => 8,
        _ => return None,
    })
}

/// One big-endian value of type `t` from the front of `b` (which holds at
/// least `scalar_size(t)` bytes).
fn read_scalar(t: u8, b: &[u8]) -> f64 {
    let a2 = || [b[0], b[1]];
    let a4 = || [b[0], b[1], b[2], b[3]];
    let a8 = || [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
    match t {
        b'b' => b[0] as i8 as f64,
        b'B' | b'c' => b[0] as f64,
        b's' => i16::from_be_bytes(a2()) as f64,
        b'S' => u16::from_be_bytes(a2()) as f64,
        b'l' => i32::from_be_bytes(a4()) as f64,
        b'L' => u32::from_be_bytes(a4()) as f64,
        b'f' => f32::from_be_bytes(a4()) as f64,
        // Q15.16 / Q31.32 fixed point.
        b'q' => i32::from_be_bytes(a4()) as f64 / 65_536.0,
        b'Q' => i64::from_be_bytes(a8()) as f64 / 4_294_967_296.0,
        b'd' => f64::from_be_bytes(a8()),
        b'j' => i64::from_be_bytes(a8()) as f64,
        b'J' => u64::from_be_bytes(a8()) as f64,
        _ => 0.0,
    }
}

/// Every value of a numeric (non-string, non-nested) KLV, whatever its
/// type: `SCAL`, `TSMP`, `STMP`, `GPSF`, `GPSP`, ...
pub fn numeric_values(klv: &Klv<'_>) -> Option<Vec<f64>> {
    if klv.type_byte == b'c' {
        return None;
    }
    let size = scalar_size(klv.type_byte)?;
    Some(
        klv.payload
            .chunks_exact(size)
            .map(|c| read_scalar(klv.type_byte, c))
            .collect(),
    )
}

/// Expand a complex-type descriptor (`TYPE`, e.g. `"lllllllSS"` for
/// `GPS9`, or `"f[3]L"`) into one type character per field.
pub fn expand_type_desc(desc: &str) -> Vec<u8> {
    let bytes = desc.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let t = bytes[i];
        i += 1;
        if i < bytes.len() && bytes[i] == b'[' {
            let close = bytes[i..].iter().position(|&b| b == b']').map(|p| i + p);
            let count = close
                .and_then(|c| std::str::from_utf8(&bytes[i + 1..c]).ok())
                .and_then(|n| n.trim().parse::<usize>().ok())
                .unwrap_or(1);
            out.extend(std::iter::repeat(t).take(count));
            i = close.map(|c| c + 1).unwrap_or(bytes.len());
        } else {
            out.push(t);
        }
    }
    out
}

/// Decode a data KLV into `(fields per sample, values)`, raw (unscaled).
/// Plain types repeat their one type across the struct (`GPS5` = 5 × `l`);
/// complex `?` entries follow the stream's `TYPE` descriptor.
fn decode_fields(klv: &Klv<'_>, type_desc: Option<&[u8]>) -> Option<(usize, Vec<f64>)> {
    let struct_size = klv.struct_size as usize;
    if struct_size == 0 || klv.is_nested() {
        return None;
    }
    let layout: Vec<u8> = if klv.type_byte == b'?' {
        let desc = type_desc?;
        let size: usize = desc.iter().map(|&t| scalar_size(t)).sum::<Option<usize>>()?;
        if size != struct_size || desc.contains(&b'c') {
            return None;
        }
        desc.to_vec()
    } else {
        let size = scalar_size(klv.type_byte)?;
        if klv.type_byte == b'c' || struct_size % size != 0 {
            return None;
        }
        vec![klv.type_byte; struct_size / size]
    };
    let mut values = Vec::with_capacity(klv.repeat as usize * layout.len());
    for sample in klv.payload.chunks_exact(struct_size) {
        let mut off = 0;
        for &t in &layout {
            let size = scalar_size(t).unwrap_or(1);
            values.push(read_scalar(t, &sample[off..off + size]));
            off += size;
        }
    }
    Some((layout.len(), values))
}

/// Divide each field by its `SCAL`: one scale for every field, or one per
/// field. A missing or zero scale leaves the value as is.
fn apply_scal(fields: usize, values: &mut [f64], scal: &[f64]) {
    if scal.is_empty() || fields == 0 {
        return;
    }
    for (i, v) in values.iter_mut().enumerate() {
        let s = if scal.len() == fields { scal[i % fields] } else { scal[0] };
        if s != 0.0 {
            *v /= s;
        }
    }
}

/// One stream's entry in one DEVC payload: its data plus the sticky
/// values that describe it.
#[derive(Debug, Clone)]
struct StreamPayload<'a> {
    data: Klv<'a>,
    scal: Vec<f64>,
    type_desc: Option<Vec<u8>>,
    tsmp: Option<u32>,
    stmp_us: Option<u64>,
    /// `GPSF`: 0 = no lock, 2 = 2D, 3 = 3D (GPS5 streams).
    gps_fix: Option<u32>,
    /// `GPSP`: dilution of precision × 100 (GPS5 streams).
    gps_precision: Option<u32>,
}

/// The STRM of `devc` that carries `key`, with its `SCAL`, `TYPE`,
/// timing stamps and GPS quality values.
fn stream_payload<'a>(devc: Klv<'a>, key: &FourCC) -> Option<StreamPayload<'a>> {
    let first = |k: &Klv<'_>| numeric_values(k).and_then(|v| v.first().copied());
    for strm in devc.iter_nested()?.filter(|k| &k.key == b"STRM" && k.is_nested()) {
        let mut data = None;
        let mut scal = Vec::new();
        let mut type_desc = None;
        let (mut tsmp, mut stmp_us, mut gps_fix, mut gps_precision) = (None, None, None, None);
        for child in strm.iter_nested()? {
            match &child.key {
                b"SCAL" => scal = numeric_values(&child).unwrap_or_default(),
                b"TYPE" => type_desc = child.as_str().map(expand_type_desc),
                b"TSMP" => tsmp = first(&child).map(|v| v as u32),
                b"STMP" => stmp_us = first(&child).map(|v| v as u64),
                b"GPSF" => gps_fix = first(&child).map(|v| v as u32),
                b"GPSP" => gps_precision = first(&child).map(|v| v as u32),
                k if k == key => data = Some(child),
                _ => {}
            }
        }
        if let Some(data) = data {
            return Some(StreamPayload { data, scal, type_desc, tsmp, stmp_us, gps_fix, gps_precision });
        }
    }
    None
}

/// One stream's samples across a whole file, payload by payload: `fields`
/// scaled values per sample.
#[derive(Debug, Clone, Default)]
pub struct StreamSeries {
    pub fields: usize,
    pub values: Vec<f64>,
    /// One entry per DEVC payload (`samples = 0` where the stream is
    /// absent), for [`sample_times`].
    pub timing: Vec<PayloadTiming>,
    /// Per payload, parallel to `timing`: `GPSF` and `GPSP` when present.
    pub gps_fix: Vec<Option<u32>>,
    pub gps_precision: Vec<Option<u32>>,
}

impl StreamSeries {
    pub fn len(&self) -> usize {
        self.values.len() / self.fields.max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn sample(&self, i: usize) -> &[f64] {
        &self.values[i * self.fields..(i + 1) * self.fields]
    }
}

/// Every sample of the stream `key` (`b"GYRO"`, `b"GPS5"`, ...), scaled
/// by its `SCAL`. `None` when no payload carries the stream.
pub fn extract_stream(bytes: &[u8], key: &FourCC) -> Option<StreamSeries> {
    let mut out = StreamSeries::default();
    let mut found = false;
    for payload in iter_payloads(bytes) {
        let mut timing = PayloadTiming::default();
        let (mut fix, mut precision) = (None, None);
        if let Some(sp) = stream_payload(payload.devc, key) {
            if let Some((fields, mut values)) = decode_fields(&sp.data, sp.type_desc.as_deref()) {
                if out.fields == 0 {
                    out.fields = fields;
                }
                // A payload laid out differently from the first is skipped
                // rather than misread.
                if fields == out.fields && !values.is_empty() {
                    apply_scal(fields, &mut values, &sp.scal);
                    timing.samples = (values.len() / fields) as u32;
                    out.values.extend(values);
                    found = true;
                }
            }
            timing.tsmp = sp.tsmp;
            timing.stmp_us = sp.stmp_us;
            fix = sp.gps_fix;
            precision = sp.gps_precision;
        }
        out.timing.push(timing);
        out.gps_fix.push(fix);
        out.gps_precision.push(precision);
    }
    found.then_some(out)
}

// ---------- Sample timing ----------

/// How one stream's samples sit in one DEVC payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PayloadTiming {
    /// The stream's samples in this payload (0 when it has none).
    pub samples: u32,
    /// `STMP`: microsecond time of the payload's first sample on the
    /// camera's clock (which runs across a recording's chapters).
    pub stmp_us: Option<u64>,
    /// `TSMP`: samples the stream has delivered so far, this payload's
    /// included — a jump larger than `samples` means samples were dropped.
    pub tsmp: Option<u32>,
}

/// Each sample's time, in seconds from the start of a file
/// `file_duration_secs` long, for a stream whose payloads are `payloads`
/// (one entry per DEVC payload, in order). Uses the best timing the stream
/// carries:
///
/// 1. `STMP` on every payload with samples (increasing): each payload's
///    samples start at its stamp and are spaced to the next stamp; the
///    first stamped payload is anchored at its place on the payload grid
///    (the camera's clock starts with the recording, not with this
///    chapter file).
/// 2. `TSMP` on every payload with samples: samples sit at their overall
///    index ÷ the stream's rate, so dropped samples leave a gap instead
///    of squeezing the rest together.
/// 3. Otherwise even spacing: payload `i` of `n` covers
///    `[i, i+1) × duration / n` and its samples are spread across it.
///
/// Times are clamped to `[0, duration]`.
pub fn sample_times(payloads: &[PayloadTiming], file_duration_secs: f32) -> Vec<f32> {
    let n = payloads.len();
    let total: usize = payloads.iter().map(|p| p.samples as usize).sum();
    if n == 0 || total == 0 {
        return Vec::new();
    }
    let dur = if file_duration_secs > 0.0 { file_duration_secs } else { n as f32 };
    let payload_secs = dur / n as f32;
    let used: Vec<usize> = (0..n).filter(|&i| payloads[i].samples > 0).collect();

    let mut out = times_from_stmp(payloads, &used, payload_secs)
        .or_else(|| times_from_tsmp(payloads, &used, payload_secs))
        .unwrap_or_else(|| {
            let mut t = Vec::with_capacity(total);
            for (i, p) in payloads.iter().enumerate() {
                for j in 0..p.samples {
                    let frac = j as f32 / p.samples as f32;
                    t.push(i as f32 * payload_secs + frac * payload_secs);
                }
            }
            t
        });
    for t in &mut out {
        *t = t.clamp(0.0, dur);
    }
    out
}

fn times_from_stmp(payloads: &[PayloadTiming], used: &[usize], payload_secs: f32) -> Option<Vec<f32>> {
    let stamps: Vec<u64> = used.iter().map(|&i| payloads[i].stmp_us).collect::<Option<_>>()?;
    if stamps.windows(2).any(|w| w[1] <= w[0]) {
        return None;
    }
    // Sample spacing within each payload: the gap to the next payload's
    // stamp over this payload's samples (only between adjacent payloads).
    let gaps: Vec<Option<f64>> = (0..used.len())
        .map(|k| {
            let next = *used.get(k + 1)?;
            (next == used[k] + 1).then(|| {
                (stamps[k + 1] - stamps[k]) as f64 / 1e6 / payloads[used[k]].samples as f64
            })
        })
        .collect();
    let mut known: Vec<f64> = gaps.iter().flatten().copied().collect();
    known.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let typical = known.get(known.len() / 2).copied();
    let anchor = used[0] as f64 * payload_secs as f64;
    let mut out = Vec::new();
    for (k, &i) in used.iter().enumerate() {
        let samples = payloads[i].samples;
        let fallback = typical.unwrap_or(payload_secs as f64 / samples as f64);
        // A gap far from the typical one (a stalled payload) uses the typical.
        let spacing = match (gaps[k], typical) {
            (Some(g), Some(t)) if g >= 0.5 * t && g <= 2.0 * t => g,
            (Some(g), None) => g,
            _ => fallback,
        };
        let start = anchor + (stamps[k] - stamps[0]) as f64 / 1e6;
        for j in 0..samples {
            out.push((start + j as f64 * spacing) as f32);
        }
    }
    Some(out)
}

fn times_from_tsmp(payloads: &[PayloadTiming], used: &[usize], payload_secs: f32) -> Option<Vec<f32>> {
    let counts: Vec<u32> = used.iter().map(|&i| payloads[i].tsmp).collect::<Option<_>>()?;
    if counts.windows(2).any(|w| w[1] < w[0]) {
        return None;
    }
    // Index (on the stream's own count) of this file's first sample.
    let first = counts[0].checked_sub(payloads[used[0]].samples)?;
    let last = *counts.last()?;
    let span = last.checked_sub(first)? as f64;
    let (lo, hi) = (used[0], *used.last()?);
    let covered = (hi + 1 - lo) as f64 * payload_secs as f64;
    if span <= 0.0 || covered <= 0.0 {
        return None;
    }
    let rate = span / covered;
    let anchor = lo as f64 * payload_secs as f64;
    let mut out = Vec::new();
    for (k, &i) in used.iter().enumerate() {
        let samples = payloads[i].samples;
        let start = counts[k].checked_sub(samples)?.checked_sub(first)? as f64;
        for j in 0..samples {
            out.push((anchor + (start + j as f64) / rate) as f32);
        }
    }
    Some(out)
}

// ---------- Gyroscope ----------

/// Gyroscope samples in rad/s, `(x, y, z)` in the camera's axes.
#[derive(Debug, Clone, Default)]
pub struct GyroSeries {
    pub times_secs: Vec<f32>,
    pub rad_s: Vec<[f32; 3]>,
}

impl GyroSeries {
    /// Average runs of consecutive samples (vector mean, mean time) down
    /// to at most `max_hz`. Rotation is scored on the mean angular
    /// velocity over a quarter second, so 25 Hz loses nothing and keeps a
    /// card's worth of 200 Hz gyro out of memory.
    pub fn decimated(&self, max_hz: f32) -> GyroSeries {
        let n = self.rad_s.len().min(self.times_secs.len());
        let span = if n > 1 { self.times_secs[n - 1] - self.times_secs[0] } else { 0.0 };
        if n < 2 || !(span > 0.0) || max_hz <= 0.0 {
            return self.clone();
        }
        let rate = (n - 1) as f32 / span;
        let block = (rate / max_hz).floor().max(1.0) as usize;
        if block == 1 {
            return self.clone();
        }
        let mut out = GyroSeries::default();
        for (t, w) in self.times_secs[..n].chunks(block).zip(self.rad_s[..n].chunks(block)) {
            let k = t.len() as f32;
            out.times_secs.push(t.iter().sum::<f32>() / k);
            let mut m = [0.0_f32; 3];
            for v in w {
                for a in 0..3 {
                    m[a] += v[a] / k;
                }
            }
            out.rad_s.push(m);
        }
        out
    }
}

/// The `GYRO` stream of a file `file_duration_secs` long, timed by
/// [`sample_times`]. `None` when the camera recorded no gyro.
pub fn extract_gyro(bytes: &[u8], file_duration_secs: f32) -> Option<GyroSeries> {
    let s = extract_stream(bytes, b"GYRO")?;
    if s.fields < 3 {
        return None;
    }
    let mut times_secs = sample_times(&s.timing, file_duration_secs);
    let rad_s: Vec<[f32; 3]> = s
        .values
        .chunks_exact(s.fields)
        .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32])
        .collect();
    times_secs.truncate(rad_s.len());
    Some(GyroSeries { times_secs, rad_s })
}

// ---------- GPS ----------

/// Which GPS stream a file carried: `GPS5` (HERO5–HERO10: lat, lon, alt,
/// 2D speed, 3D speed; fix and precision once per payload in `GPSF` /
/// `GPSP`) or `GPS9` (HERO11 and later: the same five plus days since
/// 2000, seconds since midnight, DOP and fix on every sample).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpsKind {
    Gps5,
    Gps9,
}

impl GpsKind {
    /// The stream's FourCC, as GoPro names it.
    pub fn key(self) -> &'static str {
        match self {
            GpsKind::Gps5 => "GPS5",
            GpsKind::Gps9 => "GPS9",
        }
    }
}

/// One GPS sample, scaled to real units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpsSample {
    /// Seconds from the start of the file.
    pub t_secs: f32,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f32,
    /// Ground speed, m/s.
    pub speed_2d_mps: f32,
    pub speed_3d_mps: f32,
    /// 0 = no lock, 2 = 2D, 3 = 3D; `None` when the stream didn't say.
    pub fix: Option<u32>,
    /// Dilution of precision (GoPro's `GPSP` ÷ 100); `None` when unknown.
    pub dop: Option<f32>,
}

/// A file's usable GPS samples.
#[derive(Debug, Clone)]
pub struct GpsSeries {
    pub kind: GpsKind,
    /// Samples with a lock and a sane precision, in time order.
    pub samples: Vec<GpsSample>,
    /// Samples dropped as unreliable (no lock, poor precision, nonsense).
    pub dropped: usize,
}

/// Fixes below this (no lock) are dropped: GoPro's `GPSF` is 0, 2 or 3.
pub const GPS_MIN_FIX: u32 = 2;
/// Samples whose dilution of precision is above this are dropped. GoPro's
/// guidance is that `GPSP` (DOP × 100) under 500 is a good fix.
pub const GPS_MAX_DOP: f32 = 5.0;
/// Faster than this (360 km/h) is a glitch, not a ride.
pub const GPS_MAX_SPEED_MPS: f32 = 100.0;

/// Whether a GPS sample is trustworthy enough to score on: it has a 2D or
/// 3D lock and a DOP of at most [`GPS_MAX_DOP`] when the stream reports
/// them, a plausible position and a plausible speed.
pub fn gps_sample_ok(s: &GpsSample) -> bool {
    if s.fix.is_some_and(|f| f < GPS_MIN_FIX) {
        return false;
    }
    if s.dop.is_some_and(|d| !(d <= GPS_MAX_DOP)) {
        return false;
    }
    let speed_ok = |v: f32| v.is_finite() && (0.0..=GPS_MAX_SPEED_MPS).contains(&v);
    let position_ok = s.lat_deg.is_finite()
        && s.lon_deg.is_finite()
        && s.lat_deg.abs() <= 90.0
        && s.lon_deg.abs() <= 180.0
        && !(s.lat_deg == 0.0 && s.lon_deg == 0.0);
    position_ok && speed_ok(s.speed_2d_mps)
}

/// The GPS samples of a file `file_duration_secs` long — from `GPS9` when
/// the camera wrote it, else `GPS5` — scaled, timed by [`sample_times`]
/// and filtered by [`gps_sample_ok`]. `None` when there is no GPS stream.
pub fn extract_gps(bytes: &[u8], file_duration_secs: f32) -> Option<GpsSeries> {
    if let Some(s) = extract_stream(bytes, b"GPS9").filter(|s| s.fields >= 9) {
        return Some(gps_series(GpsKind::Gps9, &s, file_duration_secs));
    }
    let s = extract_stream(bytes, b"GPS5").filter(|s| s.fields >= 5)?;
    Some(gps_series(GpsKind::Gps5, &s, file_duration_secs))
}

fn gps_series(kind: GpsKind, s: &StreamSeries, file_duration_secs: f32) -> GpsSeries {
    let times = sample_times(&s.timing, file_duration_secs);
    let mut samples = Vec::new();
    let mut dropped = 0;
    let mut i = 0;
    for (p, timing) in s.timing.iter().enumerate() {
        for _ in 0..timing.samples {
            if i >= s.len() || i >= times.len() {
                break;
            }
            let v = s.sample(i);
            let (fix, dop) = match kind {
                GpsKind::Gps9 => (Some(v[8].round().max(0.0) as u32), Some(v[7] as f32)),
                GpsKind::Gps5 => (s.gps_fix[p], s.gps_precision[p].map(|d| d as f32 / 100.0)),
            };
            let sample = GpsSample {
                t_secs: times[i],
                lat_deg: v[0],
                lon_deg: v[1],
                alt_m: v[2] as f32,
                speed_2d_mps: v[3] as f32,
                speed_3d_mps: v[4] as f32,
                fix,
                dop,
            };
            i += 1;
            if gps_sample_ok(&sample) {
                samples.push(sample);
            } else {
                dropped += 1;
            }
        }
    }
    GpsSeries { kind, samples, dropped }
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

    // ---------- synthetic GPMF for the telemetry streams ----------

    /// A nested KLV (`STRM`, `DEVC`) around already-padded children.
    fn nest(key: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
        let body = children.concat();
        klv(key, 0, 1, body.len() as u16, &body)
    }

    fn be_i32(v: &[i32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    fn be_i16(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    fn tsmp(n: u32) -> Vec<u8> {
        klv(b"TSMP", b'L', 4, 1, &n.to_be_bytes())
    }

    fn stmp(us: u64) -> Vec<u8> {
        klv(b"STMP", b'J', 8, 1, &us.to_be_bytes())
    }

    /// GPS5 rows `(lat°, lon°, alt m, 2D m/s, 3D m/s)` with HERO5–10's
    /// scales: 1e7, 1e7, 1000, 1000, 100.
    fn gps5(rows: &[(f64, f64, f64, f64, f64)]) -> Vec<u8> {
        let mut raw = Vec::new();
        for &(lat, lon, alt, s2, s3) in rows {
            raw.extend(be_i32(&[
                (lat * 1e7) as i32,
                (lon * 1e7) as i32,
                (alt * 1000.0) as i32,
                (s2 * 1000.0).round() as i32,
                (s3 * 100.0).round() as i32,
            ]));
        }
        klv(b"GPS5", b'l', 20, rows.len() as u16, &raw)
    }

    fn gps5_scal() -> Vec<u8> {
        klv(b"SCAL", b'l', 4, 5, &be_i32(&[10_000_000, 10_000_000, 1000, 1000, 100]))
    }

    /// One HERO5–10 GPS payload: fix, precision (DOP × 100), samples.
    fn gps5_payload(fix: u32, gpsp: u16, rows: &[(f64, f64, f64, f64, f64)], extra: &[Vec<u8>]) -> Vec<u8> {
        let mut children = vec![
            klv(b"STNM", b'c', 1, 4, b"GPS!"),
            klv(b"GPSF", b'L', 4, 1, &fix.to_be_bytes()),
            klv(b"GPSP", b'S', 2, 1, &gpsp.to_be_bytes()),
            gps5_scal(),
        ];
        children.extend(extra.iter().cloned());
        children.push(gps5(rows));
        nest(b"DEVC", &[nest(b"STRM", &children)])
    }

    /// A GPS9 STRM: rows `(speed 2D m/s, DOP, fix)` at a fixed position, in
    /// the HERO11+ layout `lllllllSS` with scales 1e7, 1e7, 1000, 1000,
    /// 100, 1, 1000, 100, 1.
    fn gps9_strm(rows: &[(f64, f64, u16)], extra: &[Vec<u8>]) -> Vec<u8> {
        let mut raw = Vec::new();
        for (k, &(speed, dop, fix)) in rows.iter().enumerate() {
            raw.extend(be_i32(&[
                456_789_012,              // 45.6789012°
                67_890_123,               // 6.7890123°
                1_234_000,                // 1234 m
                (speed * 1000.0) as i32,  // 2D speed
                (speed * 100.0) as i32,   // 3D speed
                9_000,                    // days since 2000
                43_200_000 + k as i32 * 100, // seconds since midnight (ms)
            ]));
            raw.extend(((dop * 100.0) as u16).to_be_bytes());
            raw.extend(fix.to_be_bytes());
        }
        let mut children = vec![
            klv(b"STNM", b'c', 1, 3, b"GPS"),
            klv(b"TYPE", b'c', 1, 9, b"lllllllSS"),
            klv(b"SCAL", b'l', 4, 9, &be_i32(&[10_000_000, 10_000_000, 1000, 1000, 100, 1, 1000, 100, 1])),
        ];
        children.extend(extra.iter().cloned());
        children.push(klv(b"GPS9", b'?', 32, rows.len() as u16, &raw));
        nest(b"STRM", &children)
    }

    fn gps9_payload(rows: &[(f64, f64, u16)], extra: &[Vec<u8>]) -> Vec<u8> {
        nest(b"DEVC", &[gps9_strm(rows, extra)])
    }

    #[test]
    fn gps5_speeds_are_scaled_to_metres_per_second() {
        let row = (45.5, 6.25, 812.0, 10.556, 10.61);
        let bytes = gps5_payload(3, 150, &[row; 18], &[]);
        let gps = extract_gps(&bytes, 1.0).expect("a GPS stream");
        assert_eq!(gps.kind, GpsKind::Gps5);
        assert_eq!(gps.samples.len(), 18);
        assert_eq!(gps.dropped, 0);
        let s = gps.samples[0];
        assert!((s.lat_deg - 45.5).abs() < 1e-6 && (s.lon_deg - 6.25).abs() < 1e-6, "{s:?}");
        assert!((s.alt_m - 812.0).abs() < 1e-3);
        assert!((s.speed_2d_mps - 10.556).abs() < 1e-4, "{}", s.speed_2d_mps);
        assert!((s.speed_2d_mps * 3.6 - 38.0).abs() < 0.1, "38 km/h");
        assert!((s.speed_3d_mps - 10.61).abs() < 1e-4);
        assert_eq!(s.fix, Some(3));
        assert_eq!(s.dop, Some(1.5));
        // 18 samples evenly across the one-second payload.
        assert!(s.t_secs.abs() < 1e-6);
        assert!((gps.samples[9].t_secs - 0.5).abs() < 1e-4);
    }

    #[test]
    fn gps5_drops_payloads_without_a_lock_or_with_poor_precision() {
        let row = (45.5, 6.25, 812.0, 8.0, 8.0);
        let bytes = [
            gps5_payload(3, 120, &[row; 10], &[]), // good 3D fix
            gps5_payload(0, 120, &[row; 10], &[]), // no lock
            gps5_payload(2, 480, &[row; 10], &[]), // 2D, DOP 4.8: kept
            gps5_payload(3, 950, &[row; 10], &[]), // DOP 9.5: dropped
        ]
        .concat();
        let gps = extract_gps(&bytes, 4.0).unwrap();
        assert_eq!(gps.samples.len(), 20);
        assert_eq!(gps.dropped, 20);
        // The kept samples are payloads 0 and 2, at their own times.
        assert!(gps.samples[..10].iter().all(|s| s.t_secs < 1.0));
        assert!(gps.samples[10..].iter().all(|s| (2.0..3.0).contains(&s.t_secs)), "{:?}", gps.samples[10]);
        // A zero position and an absurd speed are glitches too.
        let mut s = gps.samples[0];
        assert!(gps_sample_ok(&s));
        s.lat_deg = 0.0;
        s.lon_deg = 0.0;
        assert!(!gps_sample_ok(&s));
        let mut s = gps.samples[0];
        s.speed_2d_mps = 250.0;
        assert!(!gps_sample_ok(&s));
    }

    #[test]
    fn gps9_reads_per_sample_fix_and_dop() {
        let rows = [
            (12.5, 1.2, 3), // good
            (12.6, 1.2, 0), // no lock
            (12.7, 6.5, 3), // DOP 6.5
            (12.8, 2.0, 2), // 2D lock: kept
        ];
        let bytes = gps9_payload(&rows, &[]);
        let gps = extract_gps(&bytes, 1.0).unwrap();
        assert_eq!(gps.kind, GpsKind::Gps9);
        assert_eq!(gps.dropped, 2);
        let speeds: Vec<f32> = gps.samples.iter().map(|s| s.speed_2d_mps).collect();
        assert_eq!(speeds.len(), 2);
        assert!((speeds[0] - 12.5).abs() < 1e-4 && (speeds[1] - 12.8).abs() < 1e-4, "{speeds:?}");
        let s = gps.samples[1];
        assert_eq!((s.fix, s.dop), (Some(2), Some(2.0)));
        assert!((s.lat_deg - 45.6789012).abs() < 1e-6 && (s.alt_m - 1234.0).abs() < 1e-3);
        assert!((s.t_secs - 0.75).abs() < 1e-4, "4 samples over 1 s: the 4th at 0.75 s, got {}", s.t_secs);
    }

    #[test]
    fn gps9_is_preferred_when_a_camera_writes_both() {
        let gps5_strm = nest(
            b"STRM",
            &[klv(b"GPSF", b'L', 4, 1, &3u32.to_be_bytes()), gps5_scal(), gps5(&[(1.0, 1.0, 0.0, 3.0, 3.0)])],
        );
        let bytes = nest(b"DEVC", &[gps5_strm, gps9_strm(&[(20.0, 1.0, 3)], &[])]);
        let gps = extract_gps(&bytes, 1.0).unwrap();
        assert_eq!(gps.kind, GpsKind::Gps9);
        assert!((gps.samples[0].speed_2d_mps - 20.0).abs() < 1e-4);
    }

    #[test]
    fn no_gps_stream_is_none_not_empty() {
        let accl = nest(b"DEVC", &[nest(b"STRM", &[klv(b"ACCL", b's', 6, 1, &be_i16(&[0, 0, 418]))])]);
        assert!(extract_gps(&accl, 1.0).is_none());
        assert!(extract_gyro(&accl, 1.0).is_none());
    }

    /// One GYRO payload: `n` samples of `(x, y, z)` raw counts, SCAL 939
    /// (HERO-style: rad/s = raw / 939).
    fn gyro_payload(n: usize, raw: [i16; 3], extra: &[Vec<u8>]) -> Vec<u8> {
        let mut children = vec![klv(b"SCAL", b's', 2, 1, &be_i16(&[939]))];
        children.extend(extra.iter().cloned());
        let samples: Vec<i16> = (0..n).flat_map(|_| raw).collect();
        children.push(klv(b"GYRO", b's', 6, n as u16, &be_i16(&samples)));
        nest(b"DEVC", &[nest(b"STRM", &children)])
    }

    #[test]
    fn gyro_is_scaled_to_rad_per_second() {
        let bytes = [gyro_payload(200, [0, 0, 939 * 5], &[]), gyro_payload(200, [939, -939, 0], &[])].concat();
        let g = extract_gyro(&bytes, 2.0).unwrap();
        assert_eq!(g.rad_s.len(), 400);
        assert_eq!(g.times_secs.len(), 400);
        assert_eq!(g.rad_s[0], [0.0, 0.0, 5.0]);
        assert_eq!(g.rad_s[399], [1.0, -1.0, 0.0]);
        assert!((g.times_secs[200] - 1.0).abs() < 1e-5);
        assert!((g.times_secs[100] - 0.5).abs() < 1e-5);
        // Decimated to 25 Hz: blocks of 8 averaged, the spin rate kept.
        let d = g.decimated(25.0);
        assert_eq!(d.rad_s.len(), 50);
        assert_eq!(d.rad_s[0], [0.0, 0.0, 5.0]);
        assert_eq!(d.rad_s[49], [1.0, -1.0, 0.0]);
        assert!((d.times_secs[0] - 3.5 / 200.0).abs() < 1e-5, "{}", d.times_secs[0]);
        // Vibration averages out within a block.
        let buzz = GyroSeries {
            times_secs: (0..400).map(|i| i as f32 / 200.0).collect(),
            rad_s: (0..400).map(|i| if i % 2 == 0 { [3.0, 0.0, 0.0] } else { [-3.0, 0.0, 0.0] }).collect(),
        };
        assert!(buzz.decimated(25.0).rad_s.iter().all(|w| w[0].abs() < 1e-6));
        // Already slow enough: unchanged.
        assert_eq!(d.decimated(25.0).rad_s, d.rad_s);
    }

    #[test]
    fn stmp_places_samples_by_the_camera_clock() {
        // The camera clock runs from the recording, not this chapter file:
        // chapter 2 starts at 600 s. Payload 1 is 1.2 s after payload 0
        // (not the even 1.0 s), so its samples are spaced 1.2 / 4.
        let base = 600_000_000u64;
        let bytes = [
            gyro_payload(4, [0, 0, 939], &[stmp(base)]),
            gyro_payload(4, [0, 0, 939], &[stmp(base + 1_200_000)]),
            gyro_payload(4, [0, 0, 939], &[stmp(base + 2_200_000)]),
        ]
        .concat();
        let g = extract_gyro(&bytes, 3.0).unwrap();
        let t = &g.times_secs;
        assert!(t[0].abs() < 1e-6, "anchored at the file start: {t:?}");
        assert!((t[1] - 0.3).abs() < 1e-5, "{t:?}");
        assert!((t[4] - 1.2).abs() < 1e-5, "{t:?}");
        assert!((t[5] - 1.45).abs() < 1e-5, "{t:?}");
        assert!((t[8] - 2.2).abs() < 1e-5, "{t:?}");
        // The last payload has no next stamp: it takes the typical
        // (median) spacing, 0.3 s; the end clamps to the file.
        assert!((t[9] - 2.5).abs() < 1e-5, "{t:?}");
        assert_eq!(t[11], 3.0);
    }

    #[test]
    fn stmp_anchors_a_stream_that_starts_late_on_the_payload_grid() {
        let payloads = [
            PayloadTiming { samples: 0, stmp_us: None, tsmp: None },
            PayloadTiming { samples: 0, stmp_us: None, tsmp: None },
            PayloadTiming { samples: 2, stmp_us: Some(5_000_000), tsmp: None },
            PayloadTiming { samples: 2, stmp_us: Some(6_000_000), tsmp: None },
        ];
        let t = sample_times(&payloads, 4.0);
        assert_eq!(t, vec![2.0, 2.5, 3.0, 3.5]);
    }

    #[test]
    fn tsmp_leaves_a_gap_for_dropped_samples() {
        // 10 Hz stream; payload 1 lost 5 of its 10 samples (TSMP jumps by
        // 10 but only 5 arrived), so payload 2 still starts at 2.0 s.
        let payloads = [
            PayloadTiming { samples: 10, stmp_us: None, tsmp: Some(10) },
            PayloadTiming { samples: 5, stmp_us: None, tsmp: Some(20) },
            PayloadTiming { samples: 10, stmp_us: None, tsmp: Some(30) },
        ];
        let t = sample_times(&payloads, 3.0);
        assert_eq!(t.len(), 25);
        assert!((t[9] - 0.9).abs() < 1e-5);
        assert!((t[10] - 1.5).abs() < 1e-5, "the 5 that arrived are the last 5: {t:?}");
        assert!((t[15] - 2.0).abs() < 1e-5, "{t:?}");
        // TSMP that counts from earlier chapters works the same.
        let later: Vec<PayloadTiming> =
            payloads.iter().map(|p| PayloadTiming { tsmp: p.tsmp.map(|n| n + 9_000), ..*p }).collect();
        assert_eq!(sample_times(&later, 3.0), t);
    }

    #[test]
    fn without_stamps_samples_spread_evenly_per_payload() {
        let payloads = [
            PayloadTiming { samples: 4, ..Default::default() },
            PayloadTiming { samples: 0, ..Default::default() },
            PayloadTiming { samples: 2, ..Default::default() },
        ];
        let t = sample_times(&payloads, 6.0);
        assert_eq!(t, vec![0.0, 0.5, 1.0, 1.5, 4.0, 5.0]);
        assert!(sample_times(&[], 6.0).is_empty());
    }

    #[test]
    fn accel_samples_carry_their_payload_stamps() {
        let accl = |us: u64| {
            nest(
                b"DEVC",
                &[nest(
                    b"STRM",
                    &[
                        klv(b"SCAL", b's', 2, 1, &be_i16(&[418])),
                        stmp(us),
                        tsmp(2),
                        klv(b"ACCL", b's', 6, 2, &be_i16(&[0, 0, 418, 0, 0, 836])),
                    ],
                )],
            )
        };
        let bytes = [accl(0), accl(1_500_000)].concat();
        let a = extract_accel(&bytes);
        assert_eq!(a.magnitude, vec![1.0, 2.0, 1.0, 2.0]);
        assert_eq!(a.timing[1].stmp_us, Some(1_500_000));
        let t = a.times_secs(3.0);
        assert_eq!(t, vec![0.0, 0.75, 1.5, 2.25]);
    }

    #[test]
    fn type_descriptors_expand_arrays() {
        assert_eq!(expand_type_desc("lllllllSS"), b"lllllllSS".to_vec());
        assert_eq!(expand_type_desc("f[3]L"), b"fffL".to_vec());
        assert_eq!(expand_type_desc("B[2]s[0]J"), b"BBJ".to_vec());
    }
}
