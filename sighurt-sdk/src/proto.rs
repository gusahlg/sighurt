//! Lightweight protobuf wire-format encoder/decoder.
//!
//! Produces bytes fully compatible with the Protocol Buffers binary wire
//! format (no .proto file or code-generation required).  This makes
//! protobuf the *native* serialisation layer for Sighurt WASM guest applications.
//!
//! ## Encoding
//!
//! ```rust,ignore
//! use sighurt_sdk::proto::ProtoEncoder;
//!
//! let data = ProtoEncoder::new()
//!     .string(1, "alice")
//!     .uint64(2, 42)
//!     .bool(3, true)
//!     .bytes(4, &[0xCA, 0xFE])
//!     .finish();
//! ```
//!
//! ## Decoding
//!
//! ```rust,ignore
//! use sighurt_sdk::proto::ProtoDecoder;
//!
//! let mut decoder = ProtoDecoder::new(&data);
//! while let Some(field) = decoder.next() {
//!     match field.number {
//!         1 => log(&format!("name = {}", field.as_str())),
//!         2 => log(&format!("age  = {}", field.as_u64())),
//!         _ => {}
//!     }
//! }
//! ```

// ── Wire types ───────────────────────────────────────────────────────────────

const WIRE_VARINT: u32 = 0;
const WIRE_64BIT: u32 = 1;
const WIRE_LEN: u32 = 2;
const WIRE_32BIT: u32 = 5;

// ── Varint helpers ───────────────────────────────────────────────────────────

fn encode_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        if v < 0x80 {
            buf.push(v as u8);
            return;
        }
        buf.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
}

fn decode_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *pos >= buf.len() {
            return None;
        }
        let byte = buf[*pos];
        *pos += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if byte < 0x80 {
            return Some(result);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

fn zigzag_encode(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn zigzag_decode(v: u64) -> i64 {
    ((v >> 1) as i64) ^ (-((v & 1) as i64))
}

// ── Encoder ──────────────────────────────────────────────────────────────────

/// Builds a protobuf-compatible binary message field by field.
pub struct ProtoEncoder {
    buf: Vec<u8>,
}

impl ProtoEncoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    fn tag(self, field: u32, wire: u32) -> Self {
        self.varint((u64::from(field) << 3) | u64::from(wire))
    }

    fn varint(mut self, value: u64) -> Self {
        encode_varint(&mut self.buf, value);
        self
    }

    fn raw(mut self, bytes: &[u8]) -> Self {
        self.buf.extend_from_slice(bytes);
        self
    }

    // ── Varint types ────────────────────────────────────────────────

    pub fn uint64(self, field: u32, value: u64) -> Self {
        self.tag(field, WIRE_VARINT).varint(value)
    }

    pub fn uint32(self, field: u32, value: u32) -> Self {
        self.uint64(field, value as u64)
    }

    pub fn int64(self, field: u32, value: i64) -> Self {
        self.uint64(field, value as u64)
    }

    pub fn int32(self, field: u32, value: i32) -> Self {
        self.uint64(field, value as u64)
    }

    pub fn sint64(self, field: u32, value: i64) -> Self {
        self.uint64(field, zigzag_encode(value))
    }

    pub fn sint32(self, field: u32, value: i32) -> Self {
        self.sint64(field, value as i64)
    }

    pub fn bool(self, field: u32, value: bool) -> Self {
        self.uint64(field, value as u64)
    }

    // ── Length-delimited types ───────────────────────────────────────

    pub fn bytes(self, field: u32, value: &[u8]) -> Self {
        self.tag(field, WIRE_LEN)
            .varint(value.len() as u64)
            .raw(value)
    }

    pub fn string(self, field: u32, value: &str) -> Self {
        self.bytes(field, value.as_bytes())
    }

    /// Embed a sub-message (another `ProtoEncoder`'s output).
    pub fn message(self, field: u32, msg: &ProtoEncoder) -> Self {
        self.bytes(field, &msg.buf)
    }

    // ── Fixed-width types ───────────────────────────────────────────

    pub fn fixed64(self, field: u32, value: u64) -> Self {
        self.tag(field, WIRE_64BIT).raw(&value.to_le_bytes())
    }

    pub fn sfixed64(self, field: u32, value: i64) -> Self {
        self.fixed64(field, value as u64)
    }

    pub fn double(self, field: u32, value: f64) -> Self {
        self.fixed64(field, value.to_bits())
    }

    pub fn fixed32(self, field: u32, value: u32) -> Self {
        self.tag(field, WIRE_32BIT).raw(&value.to_le_bytes())
    }

    pub fn sfixed32(self, field: u32, value: i32) -> Self {
        self.fixed32(field, value as u32)
    }

    pub fn float(self, field: u32, value: f32) -> Self {
        self.fixed32(field, value.to_bits())
    }

    // ── Finalise ────────────────────────────────────────────────────

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

impl Default for ProtoEncoder {
    fn default() -> Self {
        Self::new()
    }
}

// ── Decoder ──────────────────────────────────────────────────────────────────

/// Iterates over protobuf-encoded fields one at a time.
pub struct ProtoDecoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

/// A single decoded protobuf field.
pub struct ProtoField<'a> {
    pub number: u32,
    pub wire_type: u32,
    data: FieldData<'a>,
}

enum FieldData<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

impl<'a> ProtoField<'a> {
    pub fn as_u64(&self) -> u64 {
        match self.data {
            FieldData::Varint(v) | FieldData::Fixed64(v) => v,
            FieldData::Fixed32(v) => v.into(),
            FieldData::Bytes(b) => {
                let mut arr = [0u8; 8];
                let n = b.len().min(8);
                arr[..n].copy_from_slice(&b[..n]);
                u64::from_le_bytes(arr)
            }
        }
    }

    pub fn as_i64(&self) -> i64 {
        self.as_u64() as i64
    }

    pub fn as_u32(&self) -> u32 {
        self.as_u64() as u32
    }

    pub fn as_i32(&self) -> i32 {
        self.as_u64() as i32
    }

    pub fn as_sint64(&self) -> i64 {
        zigzag_decode(self.as_u64())
    }

    pub fn as_sint32(&self) -> i32 {
        self.as_sint64() as i32
    }

    pub fn as_bool(&self) -> bool {
        self.as_u64() != 0
    }

    pub fn as_f64(&self) -> f64 {
        match self.data {
            FieldData::Fixed64(v) => f64::from_bits(v),
            _ => self.as_u64() as f64,
        }
    }

    pub fn as_f32(&self) -> f32 {
        match self.data {
            FieldData::Fixed32(v) => f32::from_bits(v),
            _ => self.as_u64() as f32,
        }
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        match self.data {
            FieldData::Bytes(b) => b,
            _ => &[],
        }
    }

    pub fn as_str(&self) -> &'a str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }

    /// Decode this field's bytes as a nested message.
    pub fn as_message(&self) -> ProtoDecoder<'a> {
        ProtoDecoder::new(self.as_bytes())
    }
}

impl<'a> ProtoDecoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<ProtoField<'a>> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let tag = decode_varint(self.buf, &mut self.pos)?;
        let wire_type = (tag & 0x07) as u32;
        let data = match wire_type {
            WIRE_VARINT => FieldData::Varint(decode_varint(self.buf, &mut self.pos)?),
            WIRE_64BIT => FieldData::Fixed64(u64::from_le_bytes(self.take(8)?.try_into().ok()?)),
            WIRE_LEN => {
                let len = decode_varint(self.buf, &mut self.pos)?;
                FieldData::Bytes(self.take(usize::try_from(len).ok()?)?)
            }
            WIRE_32BIT => FieldData::Fixed32(u32::from_le_bytes(self.take(4)?.try_into().ok()?)),
            _ => return None, // unknown wire type — stop
        };
        Some(ProtoField {
            number: (tag >> 3) as u32,
            wire_type,
            data,
        })
    }

    /// The next `n` bytes, or `None` if the buffer ends first.
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let bytes = self.buf.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(bytes)
    }

    /// Collect all fields into a `Vec` for random-access lookup.
    pub fn collect_fields(&mut self) -> Vec<ProtoField<'a>> {
        let mut fields = Vec::new();
        while let Some(f) = self.next() {
            fields.push(f);
        }
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field<'a>(fields: &'a [ProtoField<'a>], number: u32) -> &'a ProtoField<'a> {
        fields
            .iter()
            .find(|f| f.number == number)
            .unwrap_or_else(|| panic!("missing field {number}"))
    }

    #[test]
    fn empty_message_has_no_fields() {
        let data = ProtoEncoder::new().finish();
        assert!(data.is_empty());
        assert!(ProtoDecoder::new(&data).next().is_none());
    }

    #[test]
    fn roundtrip_varint_types() {
        let data = ProtoEncoder::new()
            .uint32(1, 42)
            .uint64(2, u64::MAX)
            .int32(3, -7)
            .int64(4, i64::MIN)
            .sint32(5, -123)
            .sint64(6, i64::MIN)
            .bool(7, true)
            .bool(8, false)
            .finish();
        let fields = ProtoDecoder::new(&data).collect_fields();
        assert_eq!(fields.len(), 8);
        assert_eq!(field(&fields, 1).as_u32(), 42);
        assert_eq!(field(&fields, 2).as_u64(), u64::MAX);
        assert_eq!(field(&fields, 3).as_i32(), -7);
        assert_eq!(field(&fields, 4).as_i64(), i64::MIN);
        assert_eq!(field(&fields, 5).as_sint32(), -123);
        assert_eq!(field(&fields, 6).as_sint64(), i64::MIN);
        assert!(field(&fields, 7).as_bool());
        assert!(!field(&fields, 8).as_bool());
    }

    #[test]
    fn roundtrip_bytes_and_strings() {
        let data = ProtoEncoder::new()
            .string(1, "alice")
            .bytes(2, &[0xCA, 0xFE])
            .string(3, "")
            .bytes(4, &[])
            .string(5, "π")
            .finish();
        let fields = ProtoDecoder::new(&data).collect_fields();
        assert_eq!(field(&fields, 1).as_str(), "alice");
        assert_eq!(field(&fields, 2).as_bytes(), &[0xCA, 0xFE]);
        assert_eq!(field(&fields, 3).as_str(), "");
        assert!(field(&fields, 4).as_bytes().is_empty());
        assert_eq!(field(&fields, 5).as_str(), "π");
    }

    #[test]
    fn roundtrip_fixed_and_float() {
        let data = ProtoEncoder::new()
            .fixed32(1, 0xAABBCCDD)
            .fixed64(2, 0x1122334455667788)
            .sfixed32(3, -42)
            .sfixed64(4, i64::MIN)
            .float(5, 1.5)
            .double(6, -2.25)
            .finish();
        let fields = ProtoDecoder::new(&data).collect_fields();
        assert_eq!(field(&fields, 1).as_u32(), 0xAABBCCDD);
        assert_eq!(field(&fields, 2).as_u64(), 0x1122334455667788);
        assert_eq!(field(&fields, 3).as_i32(), -42);
        assert_eq!(field(&fields, 4).as_i64(), i64::MIN);
        assert_eq!(field(&fields, 5).as_f32(), 1.5);
        assert_eq!(field(&fields, 6).as_f64(), -2.25);
    }

    #[test]
    fn nested_message_roundtrip() {
        let inner = ProtoEncoder::new().string(1, "nested").uint32(2, 9);
        let data = ProtoEncoder::new()
            .string(1, "outer")
            .message(2, &inner)
            .finish();
        let fields = ProtoDecoder::new(&data).collect_fields();
        assert_eq!(field(&fields, 1).as_str(), "outer");
        let inner_fields = field(&fields, 2).as_message().collect_fields();
        assert_eq!(field(&inner_fields, 1).as_str(), "nested");
        assert_eq!(field(&inner_fields, 2).as_u32(), 9);
    }

    #[test]
    fn decoder_skips_unknown_field_numbers() {
        let data = ProtoEncoder::new()
            .string(1, "keep")
            .uint32(99, 7)
            .bool(2, true)
            .finish();
        let mut seen = Vec::new();
        let mut decoder = ProtoDecoder::new(&data);
        while let Some(f) = decoder.next() {
            if f.number == 1 || f.number == 2 {
                seen.push(f.number);
            }
        }
        assert_eq!(seen, vec![1, 2]);
    }

    #[test]
    fn truncated_input_stops_cleanly() {
        let data = ProtoEncoder::new().string(1, "hello").finish();
        assert!(
            ProtoDecoder::new(&data[..data.len() - 1])
                .collect_fields()
                .len()
                < 2
        );
        assert!(ProtoDecoder::new(&[]).next().is_none());
    }

    #[test]
    fn oversized_length_prefix_stops_cleanly() {
        // Field 1, length-delimited, claiming u64::MAX bytes: must not overflow or panic.
        let mut data = vec![(1 << 3) | WIRE_LEN as u8];
        encode_varint(&mut data, u64::MAX);
        assert!(ProtoDecoder::new(&data).next().is_none());
    }
}
