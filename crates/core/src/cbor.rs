//! Bounded, zero-unsafe RFC 8949 CBOR decoder for AWS Smithy RPCv2 (`rpc-v2-cbor`).
//!
//! Modern AWS SDKs (`boto3`, `aws-sdk-go-v2`, `aws-sdk-js-v3`, `aws-sdk-rust`)
//! transmit CloudWatch, SQS, SSM, DynamoDB and custom Smithy services using
//! `Content-Type: application/cbor` (`smithy-protocol: rpc-v2-cbor`). Decoding
//! CBOR into [`serde_json::Value`] lets both IAM resource derivation and the
//! desktop Inspector read every field identically to JSON protocols.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use serde_json::{Map, Number, Value};

const MAX_DEPTH: usize = 64;
const MAX_ITEMS: usize = 250_000;

/// Decode a complete CBOR value into [`serde_json::Value`].
pub fn decode_cbor(input: &[u8]) -> Option<Value> {
    if input.is_empty() {
        return None;
    }
    let mut reader = CborReader {
        buf: input,
        pos: 0,
        items: 0,
    };
    let val = reader.read_value(0)?;
    // Allow trailing padding whitespace only if entire frame was consumed;
    // CBOR frames are self-delimiting and must consume the payload cleanly.
    (reader.pos == input.len()).then_some(val)
}

/// Decode a CBOR payload into pretty-printed JSON text.
pub fn decode_cbor_pretty(input: &[u8]) -> Option<String> {
    let val = decode_cbor(input)?;
    serde_json::to_string_pretty(&val).ok()
}

/// Decode a CBOR payload into compact JSON text (used for bounded trace storage).
pub fn decode_cbor_compact(input: &[u8]) -> Option<String> {
    let val = decode_cbor(input)?;
    serde_json::to_string(&val).ok()
}

/// Encode a [`serde_json::Value`] into definite-length RFC 8949 CBOR bytes.
pub fn encode_cbor(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_value(value, &mut out);
    out
}

fn encode_value(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.push(0xf6),
        Value::Bool(false) => out.push(0xf4),
        Value::Bool(true) => out.push(0xf5),
        Value::Number(num) => {
            if let Some(u) = num.as_u64() {
                write_type_arg(out, 0, u);
            } else if let Some(i) = num.as_i64() {
                if i >= 0 {
                    write_type_arg(out, 0, i as u64);
                } else {
                    let arg = (-1i128 - (i as i128)) as u64;
                    write_type_arg(out, 1, arg);
                }
            } else if let Some(f) = num.as_f64() {
                out.push(0xfb);
                out.extend_from_slice(&f.to_be_bytes());
            } else {
                out.push(0xf6);
            }
        }
        Value::String(s) => {
            write_type_arg(out, 3, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Array(items) => {
            write_type_arg(out, 4, items.len() as u64);
            for item in items {
                encode_value(item, out);
            }
        }
        Value::Object(map) => {
            write_type_arg(out, 5, map.len() as u64);
            for (k, v) in map {
                write_type_arg(out, 3, k.len() as u64);
                out.extend_from_slice(k.as_bytes());
                encode_value(v, out);
            }
        }
    }
}

fn write_type_arg(out: &mut Vec<u8>, major: u8, arg: u64) {
    let hi = major << 5;
    match arg {
        0..=23 => out.push(hi | (arg as u8)),
        24..=0xff => {
            out.push(hi | 24);
            out.push(arg as u8);
        }
        0x100..=0xffff => {
            out.push(hi | 25);
            out.extend_from_slice(&(arg as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(hi | 26);
            out.extend_from_slice(&(arg as u32).to_be_bytes());
        }
        _ => {
            out.push(hi | 27);
            out.extend_from_slice(&arg.to_be_bytes());
        }
    }
}

struct CborReader<'a> {
    buf: &'a [u8],
    pos: usize,
    items: usize,
}

impl<'a> CborReader<'a> {
    fn bump_items(&mut self) -> Option<()> {
        self.items = self.items.checked_add(1)?;
        (self.items <= MAX_ITEMS).then_some(())
    }

    fn read_byte(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn read_slice(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn read_arg(&mut self, info: u8) -> Option<u64> {
        match info {
            0..=23 => Some(info as u64),
            24 => Some(self.read_byte()? as u64),
            25 => {
                let s = self.read_slice(2)?;
                Some(u16::from_be_bytes([s[0], s[1]]) as u64)
            }
            26 => {
                let s = self.read_slice(4)?;
                Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as u64)
            }
            27 => {
                let s = self.read_slice(8)?;
                Some(u64::from_be_bytes([
                    s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
                ]))
            }
            _ => None,
        }
    }

    fn read_value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.bump_items()?;
        let head = self.read_byte()?;
        let major = head >> 5;
        let info = head & 0x1f;

        match major {
            // 0: Unsigned integer
            0 => {
                let n = self.read_arg(info)?;
                Some(Value::Number(Number::from(n)))
            }
            // 1: Negative integer (-1 - n)
            1 => {
                let n = self.read_arg(info)?;
                if let Some(neg) = (-1i128).checked_sub(n as i128) {
                    if let Ok(i) = i64::try_from(neg) {
                        return Some(Value::Number(Number::from(i)));
                    }
                    return Some(Value::String(neg.to_string()));
                }
                None
            }
            // 2: Byte string (definite or indefinite)
            2 => {
                if info == 31 {
                    let mut acc = Vec::new();
                    while self.buf.get(self.pos).copied()? != 0xff {
                        self.bump_items()?;
                        let ch_head = self.read_byte()?;
                        if (ch_head >> 5) != 2 || (ch_head & 0x1f) == 31 {
                            return None;
                        }
                        let len = usize::try_from(self.read_arg(ch_head & 0x1f)?).ok()?;
                        acc.extend_from_slice(self.read_slice(len)?);
                    }
                    self.pos += 1; // consume 0xff break
                    Some(Value::String(BASE64.encode(&acc)))
                } else {
                    let len = usize::try_from(self.read_arg(info)?).ok()?;
                    let bytes = self.read_slice(len)?;
                    Some(Value::String(BASE64.encode(bytes)))
                }
            }
            // 3: UTF-8 text string (definite or indefinite)
            3 => {
                if info == 31 {
                    let mut acc = String::new();
                    while self.buf.get(self.pos).copied()? != 0xff {
                        self.bump_items()?;
                        let ch_head = self.read_byte()?;
                        if (ch_head >> 5) != 3 || (ch_head & 0x1f) == 31 {
                            return None;
                        }
                        let len = usize::try_from(self.read_arg(ch_head & 0x1f)?).ok()?;
                        let piece = std::str::from_utf8(self.read_slice(len)?).ok()?;
                        acc.push_str(piece);
                    }
                    self.pos += 1;
                    Some(Value::String(acc))
                } else {
                    let len = usize::try_from(self.read_arg(info)?).ok()?;
                    let s = std::str::from_utf8(self.read_slice(len)?).ok()?;
                    Some(Value::String(s.to_string()))
                }
            }
            // 4: Array
            4 => {
                let mut items = Vec::new();
                if info == 31 {
                    while self.buf.get(self.pos).copied()? != 0xff {
                        items.push(self.read_value(depth + 1)?);
                    }
                    self.pos += 1;
                } else {
                    let len = usize::try_from(self.read_arg(info)?).ok()?;
                    items.reserve(len.min(1024));
                    for _ in 0..len {
                        items.push(self.read_value(depth + 1)?);
                    }
                }
                Some(Value::Array(items))
            }
            // 5: Map
            5 => {
                let mut map = Map::new();
                if info == 31 {
                    while self.buf.get(self.pos).copied()? != 0xff {
                        let k = value_to_key(self.read_value(depth + 1)?);
                        let v = self.read_value(depth + 1)?;
                        map.insert(k, v);
                    }
                    self.pos += 1;
                } else {
                    let len = usize::try_from(self.read_arg(info)?).ok()?;
                    for _ in 0..len {
                        let k = value_to_key(self.read_value(depth + 1)?);
                        let v = self.read_value(depth + 1)?;
                        map.insert(k, v);
                    }
                }
                Some(Value::Object(map))
            }
            // 6: Semantic Tag (e.g. Tag 1 epoch timestamp in Smithy RPCv2 CBOR)
            6 => {
                let tag = self.read_arg(info)?;
                let inner = self.read_value(depth + 1)?;
                if tag == 1 {
                    if let Some(secs) = inner.as_f64() {
                        let whole = secs.trunc() as i64;
                        let nanos = ((secs.fract().abs()) * 1e9).round() as u32;
                        if let Some(dt) =
                            DateTime::<Utc>::from_timestamp(whole, nanos.min(999_999_999))
                        {
                            return Some(Value::String(dt.to_rfc3339()));
                        }
                    } else if let Some(secs) = inner.as_i64()
                        && let Some(dt) = DateTime::<Utc>::from_timestamp(secs, 0)
                    {
                        return Some(Value::String(dt.to_rfc3339()));
                    }
                }
                Some(inner)
            }
            // 7: Simple values & floats
            7 => match info {
                20 => Some(Value::Bool(false)),
                21 => Some(Value::Bool(true)),
                22 | 23 => Some(Value::Null),
                25 => {
                    let s = self.read_slice(2)?;
                    let f = f16_to_f64(u16::from_be_bytes([s[0], s[1]]));
                    Some(float_to_json(f))
                }
                26 => {
                    let s = self.read_slice(4)?;
                    let f = f32::from_be_bytes([s[0], s[1], s[2], s[3]]) as f64;
                    Some(float_to_json(f))
                }
                27 => {
                    let s = self.read_slice(8)?;
                    let f = f64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
                    Some(float_to_json(f))
                }
                0..=19 => Some(Value::Number(Number::from(info as u64))),
                24 => Some(Value::Number(Number::from(self.read_byte()? as u64))),
                _ => None,
            },
            _ => None,
        }
    }
}

fn value_to_key(val: Value) -> String {
    match val {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

fn float_to_json(f: f64) -> Value {
    if let Some(num) = Number::from_f64(f) {
        Value::Number(num)
    } else if f.is_nan() {
        Value::String("NaN".into())
    } else if f.is_sign_positive() {
        Value::String("Infinity".into())
    } else {
        Value::String("-Infinity".into())
    }
}

/// Exact IEEE-754 binary16 → binary64 conversion.
fn f16_to_f64(bits: u16) -> f64 {
    let sign = if (bits & 0x8000) != 0 { -1.0 } else { 1.0 };
    let exp = ((bits >> 10) & 0x1f) as i32;
    let mant = (bits & 0x03ff) as u32;
    if exp == 0 {
        if mant == 0 {
            sign * 0.0
        } else {
            sign * (mant as f64 / 1024.0) * 2.0_f64.powi(-14)
        }
    } else if exp == 31 {
        if mant == 0 {
            sign * f64::INFINITY
        } else {
            f64::NAN
        }
    } else {
        sign * (1.0 + mant as f64 / 1024.0) * 2.0_f64.powi(exp - 15)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_smithy_rpcv2_cbor_maps_tags_and_indefinite_collections() {
        // CBOR map: {"QueueName": "orders", "DelaySeconds": 5, "Tags": {"env": "dev"}, "At": 1(1700000000)}
        let mut frame = Vec::new();
        frame.push(0xa4); // map(4)
        // "QueueName": "orders"
        frame.push(0x69);
        frame.extend_from_slice(b"QueueName");
        frame.push(0x66);
        frame.extend_from_slice(b"orders");
        // "DelaySeconds": 5
        frame.push(0x6c);
        frame.extend_from_slice(b"DelaySeconds");
        frame.push(0x05);
        // "Tags": indefinite map {"env": "dev"}
        frame.push(0x64);
        frame.extend_from_slice(b"Tags");
        frame.push(0xbf);
        frame.push(0x63);
        frame.extend_from_slice(b"env");
        frame.push(0x63);
        frame.extend_from_slice(b"dev");
        frame.push(0xff);
        // "At": tag(1) + uint32(1700000000)
        frame.push(0x62);
        frame.extend_from_slice(b"At");
        frame.push(0xc1);
        frame.push(0x1a);
        frame.extend_from_slice(&1_700_000_000u32.to_be_bytes());

        let json = decode_cbor(&frame).expect("valid CBOR");
        assert_eq!(json["QueueName"], "orders");
        assert_eq!(json["DelaySeconds"], 5);
        assert_eq!(json["Tags"]["env"], "dev");
        assert!(json["At"].as_str().unwrap().starts_with("2023-11-14T"));

        let roundtrip_src = serde_json::json!({
            "QueueName": "orders",
            "DelaySeconds": 5,
            "Negative": -42,
            "Ratio": 1.5,
            "Active": true,
            "Empty": null,
            "Items": ["a", "b"],
        });
        let encoded = encode_cbor(&roundtrip_src);
        assert_eq!(decode_cbor(&encoded).as_ref(), Some(&roundtrip_src));
    }
}
