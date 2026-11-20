//! Local AWS SigV4 request re-signer for the Edit-and-Replay Request Composer.
//!
//! When a developer edits a captured request's JSON/XML/Query body, URI path, or
//! headers in the Composer, the original `Authorization` signature would fail
//! `SignatureDoesNotMatch` on strict emulators (AWS Emulator, LocalStack Pro).
//! This module computes a fresh RFC-compliant `AWS4-HMAC-SHA256` signature
//! locally in-memory using `ring::hmac` and `sha2::Sha256`.

use std::collections::BTreeMap;

use chrono::Utc;
use ring::hmac;
use sha2::{Digest, Sha256};

use crate::model::Header;

pub struct SignParams<'a> {
    pub method: &'a str,
    pub path_and_query: &'a str,
    pub host: &'a str,
    pub service: &'a str,
    pub region: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub session_token: Option<&'a str>,
}

/// Replace `x-amz-date`, `x-amz-content-sha256`, `host`, and `authorization` on
/// `headers` with a freshly signed `AWS4-HMAC-SHA256` header over `body`.
pub fn sign_headers(headers: &mut Vec<Header>, body: &[u8], params: &SignParams<'_>) {
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    sign_headers_at(headers, body, params, &amz_date, &date_stamp);
}

pub fn sign_headers_at(
    headers: &mut Vec<Header>,
    body: &[u8],
    params: &SignParams<'_>,
    amz_date: &str,
    date_stamp: &str,
) {
    let payload_hash = hex_sha256(body);

    // Remove stale signing / framing headers before re-signing.
    headers.retain(|h| {
        let low = h.name.to_ascii_lowercase();
        !matches!(
            low.as_str(),
            "authorization"
                | "x-amz-date"
                | "date"
                | "x-amz-content-sha256"
                | "content-length"
                | "transfer-encoding"
                | "expect"
        )
    });

    if !headers.iter().any(|h| h.name.eq_ignore_ascii_case("host")) && !params.host.is_empty() {
        headers.insert(
            0,
            Header {
                name: "host".into(),
                value: params.host.to_string(),
            },
        );
    }
    headers.push(Header {
        name: "x-amz-date".into(),
        value: amz_date.to_string(),
    });
    headers.push(Header {
        name: "x-amz-content-sha256".into(),
        value: payload_hash.clone(),
    });
    if let Some(tok) = params.session_token.filter(|s| !s.is_empty())
        && !headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("x-amz-security-token"))
    {
        headers.push(Header {
            name: "x-amz-security-token".into(),
            value: tok.to_string(),
        });
    }

    let (raw_path, raw_query) = params
        .path_and_query
        .split_once('?')
        .unwrap_or((params.path_and_query, ""));
    let canonical_uri = canonicalize_uri_path(raw_path, params.service == "s3");
    let canonical_query = canonicalize_query(raw_query);

    let mut canonical_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for h in headers.iter() {
        let low = h.name.trim().to_ascii_lowercase();
        if low.is_empty() || matches!(low.as_str(), "connection" | "user-agent") {
            continue;
        }
        canonical_map
            .entry(low)
            .or_default()
            .push(collapse_ws(h.value.trim()));
    }

    let mut canonical_headers = String::new();
    let mut signed_names: Vec<&str> = Vec::with_capacity(canonical_map.len());
    for (name, vals) in &canonical_map {
        canonical_headers.push_str(name);
        canonical_headers.push(':');
        canonical_headers.push_str(&vals.join(","));
        canonical_headers.push('\n');
        signed_names.push(name.as_str());
    }
    let signed_headers = signed_names.join(";");

    let canonical_request = format!(
        "{}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        params.method.trim().to_ascii_uppercase()
    );

    let scope = format!(
        "{date_stamp}/{}/{}/aws4_request",
        if params.region.is_empty() {
            "us-east-1"
        } else {
            params.region
        },
        if params.service.is_empty() {
            "s3"
        } else {
            params.service
        }
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );

    let k_secret = format!("AWS4{}", params.secret_key);
    let k_date = hmac_sha256(k_secret.as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(
        &k_date,
        if params.region.is_empty() {
            b"us-east-1"
        } else {
            params.region.as_bytes()
        },
    );
    let k_service = hmac_sha256(
        &k_region,
        if params.service.is_empty() {
            b"s3"
        } else {
            params.service.as_bytes()
        },
    );
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    headers.push(Header {
        name: "authorization".into(),
        value: format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            params.access_key
        ),
    });
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::sign(&signing_key, msg).as_ref().to_vec()
}

fn hex_sha256(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            prev_space = false;
            out.push(ch);
        }
    }
    out
}

fn uri_encode(input: &str, encode_slash: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if unreserved || (!encode_slash && b == b'/') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn canonicalize_uri_path(raw: &str, is_s3: bool) -> String {
    let ensured = if raw.is_empty() { "/" } else { raw };
    let decoded = lenslet_core::wire::percent_decode_path(ensured);
    if is_s3 {
        uri_encode(&decoded, false)
    } else {
        // Non-S3 SigV4 normalizes segments and encodes once.
        uri_encode(&decoded, false)
    }
}

fn canonicalize_query(raw_query: &str) -> String {
    if raw_query.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<(String, String)> = raw_query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let k_dec = lenslet_core::wire::percent_decode_query(k);
            let v_dec = lenslet_core::wire::percent_decode_query(v);
            (uri_encode(&k_dec, true), uri_encode(&v_dec, true))
        })
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_aws_sigv4_headers_deterministically() {
        let mut headers = vec![Header {
            name: "x-amz-target".into(),
            value: "DynamoDB_20120810.GetItem".into(),
        }];
        sign_headers_at(
            &mut headers,
            br#"{"TableName":"orders"}"#,
            &SignParams {
                method: "POST",
                path_and_query: "/",
                host: "127.0.0.1:9297",
                service: "dynamodb",
                region: "us-east-1",
                access_key: "lenslet-local",
                secret_key: "lenslet-local",
                session_token: None,
            },
            "20261120T000000Z",
            "20261120",
        );
        let auth = headers.iter().find(|h| h.name == "authorization").unwrap();
        assert!(auth
            .value
            .starts_with("AWS4-HMAC-SHA256 Credential=lenslet-local/20261120/us-east-1/dynamodb/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-target, Signature="));
        assert_eq!(auth.value.split("Signature=").nth(1).unwrap().len(), 64);
    }
}
