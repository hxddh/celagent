//! Zero-dependency syntax & search-match highlighter producing strictly sorted,
//! disjoint `(Range<usize>, HighlightStyle)` spans for GPUI [`StyledText`].

use std::ops::Range;

use gpui::{FontWeight, HighlightStyle, UnderlineStyle, px, rgb};

use crate::theme::{self, Palette};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxKind {
    Json,
    Xml,
    Form,
    Hex,
    Plain,
}

impl SyntaxKind {
    pub fn detect(text: &str, content_type: &str, is_hex: bool, is_form: bool) -> Self {
        if is_hex {
            return SyntaxKind::Hex;
        }
        if is_form {
            return SyntaxKind::Form;
        }
        let ct = content_type.to_ascii_lowercase();
        let trimmed = text.trim_start();
        if ct.contains("json") || trimmed.starts_with('{') || trimmed.starts_with('[') {
            SyntaxKind::Json
        } else if ct.contains("xml") || trimmed.starts_with('<') {
            SyntaxKind::Xml
        } else if ct.contains("form-urlencoded") {
            SyntaxKind::Form
        } else {
            SyntaxKind::Plain
        }
    }
}

/// Tokenize `text` according to `kind` and overlay any case-insensitive matches
/// of `search_tokens`, returning `(disjoint_sorted_spans, search_match_count)`.
pub fn highlight_code(
    text: &str,
    kind: SyntaxKind,
    search_tokens: &[String],
    p: &Palette,
) -> (Vec<(Range<usize>, HighlightStyle)>, usize) {
    // Cap syntax tokenization on huge payloads (>128 KiB) to keep 120fps scrolling smooth.
    let base = if text.len() <= 128 * 1024 {
        match kind {
            SyntaxKind::Json => highlight_json(text, p),
            SyntaxKind::Xml => highlight_xml(text, p),
            SyntaxKind::Form => highlight_form(text, p),
            SyntaxKind::Hex => highlight_hex(text, p),
            SyntaxKind::Plain => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let matches = find_search_matches(text, search_tokens);
    let count = matches.len();
    if matches.is_empty() {
        return (sanitize_spans(text, base), 0);
    }
    let merged = overlay_matches(base, &matches, p);
    (sanitize_spans(text, merged), count)
}

fn search_match_style(p: &Palette) -> HighlightStyle {
    HighlightStyle {
        color: Some(rgb(p.text).into()),
        font_weight: Some(FontWeight::SEMIBOLD),
        background_color: Some(theme::h(p.accent, 0.24)),
        underline: Some(UnderlineStyle {
            thickness: px(1.5),
            color: Some(rgb(p.accent).into()),
            wavy: false,
        }),
        ..Default::default()
    }
}

fn fg(hex: u32) -> HighlightStyle {
    HighlightStyle {
        color: Some(rgb(hex).into()),
        ..Default::default()
    }
}

fn fg_weight(hex: u32, weight: FontWeight) -> HighlightStyle {
    HighlightStyle {
        color: Some(rgb(hex).into()),
        font_weight: Some(weight),
        ..Default::default()
    }
}

fn highlight_json(text: &str, p: &Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 1;
                        if i < bytes.len() {
                            let esc_len = text[i..].chars().next().map_or(1, char::len_utf8);
                            i += esc_len;
                        }
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
                    i += ch_len;
                }
                let end = i;
                // Look ahead past ASCII whitespace for `:` to distinguish keys from string values.
                let mut k = end;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                let is_key = bytes.get(k) == Some(&b':');
                let style = if is_key {
                    fg_weight(p.accent, FontWeight::MEDIUM)
                } else {
                    fg(p.green)
                };
                out.push((start..end, style));
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                i += 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_digit()
                        || matches!(bytes[i], b'.' | b'e' | b'E' | b'+' | b'-'))
                {
                    i += 1;
                }
                out.push((start..i, fg(p.amber)));
            }
            b't' if bytes.get(i..i + 4) == Some(b"true") => {
                out.push((i..i + 4, fg_weight(p.purple, FontWeight::MEDIUM)));
                i += 4;
            }
            b'f' if bytes.get(i..i + 5) == Some(b"false") => {
                out.push((i..i + 5, fg_weight(p.purple, FontWeight::MEDIUM)));
                i += 5;
            }
            b'n' if bytes.get(i..i + 4) == Some(b"null") => {
                out.push((i..i + 4, fg_weight(p.text_muted, FontWeight::MEDIUM)));
                i += 4;
            }
            b'{' | b'}' | b'[' | b']' | b':' | b',' => {
                out.push((i..i + 1, fg(p.text_faint)));
                i += 1;
            }
            _ => {
                let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
                i += ch_len;
            }
        }
    }
    out
}

fn highlight_xml(text: &str, p: &Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if bytes.get(i..i + 4) == Some(b"<!--") {
                let start = i;
                if let Some(rel) = text[i + 4..].find("-->") {
                    i = i + 4 + rel + 3;
                } else {
                    i = bytes.len();
                }
                out.push((start..i, fg(p.text_faint)));
                continue;
            }
            let tag_start = i;
            i += 1;
            if i < bytes.len() && matches!(bytes[i], b'/' | b'?') {
                i += 1;
            }
            let name_start = i;
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && !matches!(bytes[i], b'>' | b'/' | b'?')
            {
                let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
                i += ch_len;
            }
            out.push((tag_start..name_start, fg(p.text_faint)));
            if i > name_start {
                out.push((name_start..i, fg_weight(p.accent, FontWeight::MEDIUM)));
            }
            while i < bytes.len() && bytes[i] != b'>' {
                if bytes[i] == b'"' || bytes[i] == b'\'' {
                    let q = bytes[i];
                    let q_start = i;
                    i += 1;
                    while i < bytes.len() && bytes[i] != q {
                        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
                        i += ch_len;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                    out.push((q_start..i, fg(p.green)));
                } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b':' {
                    let a_start = i;
                    while i < bytes.len()
                        && (bytes[i].is_ascii_alphanumeric()
                            || matches!(bytes[i], b'-' | b'_' | b':' | b'.'))
                    {
                        i += 1;
                    }
                    out.push((a_start..i, fg(p.purple)));
                } else {
                    let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
                    i += ch_len;
                }
            }
            if i < bytes.len() && bytes[i] == b'>' {
                out.push((i..i + 1, fg(p.text_faint)));
                i += 1;
            }
        } else {
            let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
            i += ch_len;
        }
    }
    out
}

fn highlight_form(text: &str, p: &Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent == 0
            && let Some(eq_rel) = line.find(" = ")
        {
            out.push((
                offset..offset + eq_rel,
                fg_weight(p.accent, FontWeight::MEDIUM),
            ));
            out.push((offset + eq_rel..offset + eq_rel + 3, fg(p.text_faint)));
            let val_start = offset + eq_rel + 3;
            let val_end = offset + line.trim_end_matches(['\r', '\n']).len();
            if val_end > val_start {
                out.push((val_start..val_end, fg(p.green)));
            }
        } else if trimmed.starts_with('{')
            || trimmed.starts_with('[')
            || trimmed.starts_with('"')
            || trimmed.starts_with('}')
            || trimmed.starts_with(']')
        {
            let sub = highlight_json(line, p);
            for (r, style) in sub {
                out.push((offset + r.start..offset + r.end, style));
            }
        }
        offset += line.len();
    }
    out
}

fn highlight_hex(text: &str, p: &Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let clean = line.trim_end_matches(['\r', '\n']);
        if clean.len() >= 6 && clean.is_char_boundary(4) {
            out.push((offset..offset + 4, fg(p.text_faint)));
            if let Some(bar_start) = clean.find('|') {
                out.push((offset + 4..offset + bar_start, fg(p.accent)));
                out.push((offset + bar_start..offset + clean.len(), fg(p.green)));
            }
        }
        offset += line.len();
    }
    out
}

/// Find all merged, non-overlapping UTF-8 byte ranges in `text` matching any
/// non-empty token in `tokens` (ASCII case-insensitive or Unicode lowercase match).
fn find_search_matches(text: &str, tokens: &[String]) -> Vec<Range<usize>> {
    let active: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|t| !t.is_empty())
        .collect();
    if active.is_empty() || text.is_empty() {
        return Vec::new();
    }

    let mut raw_ranges: Vec<Range<usize>> = Vec::new();
    for tok in active {
        if tok.is_ascii() {
            let needle = tok.as_bytes();
            let hay = text.as_bytes();
            if needle.len() > hay.len() {
                continue;
            }
            let mut pos = 0usize;
            while pos + needle.len() <= hay.len() {
                if hay[pos..pos + needle.len()].eq_ignore_ascii_case(needle)
                    && text.is_char_boundary(pos)
                    && text.is_char_boundary(pos + needle.len())
                {
                    raw_ranges.push(pos..pos + needle.len());
                    pos += needle.len();
                } else {
                    pos += 1;
                }
            }
        } else {
            let tok_lower = tok.to_lowercase();
            // Exact substring or case-folded match on valid char boundaries.
            for (idx, _) in text.match_indices(&tok_lower) {
                raw_ranges.push(idx..idx + tok_lower.len());
            }
            if tok != tok_lower {
                for (idx, _) in text.match_indices(tok) {
                    raw_ranges.push(idx..idx + tok.len());
                }
            }
        }
    }

    if raw_ranges.is_empty() {
        return Vec::new();
    }
    raw_ranges.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| b.end.cmp(&a.end)));

    let mut merged: Vec<Range<usize>> = Vec::with_capacity(raw_ranges.len());
    for r in raw_ranges {
        if let Some(last) = merged.last_mut()
            && r.start <= last.end
        {
            last.end = last.end.max(r.end);
        } else {
            merged.push(r);
        }
    }
    merged
}

/// Overlay `matches` on top of `base` syntax spans, splitting any overlapping
/// syntax spans cleanly so the result remains strictly sorted and disjoint.
fn overlay_matches(
    base: Vec<(Range<usize>, HighlightStyle)>,
    matches: &[Range<usize>],
    p: &Palette,
) -> Vec<(Range<usize>, HighlightStyle)> {
    overlay_matches_cursor(base, matches, search_match_style(p))
}

fn overlay_matches_cursor(
    base: Vec<(Range<usize>, HighlightStyle)>,
    matches: &[Range<usize>],
    match_style: HighlightStyle,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::with_capacity(base.len() + matches.len() * 2);
    let mut base_it = base.into_iter();
    let mut cur_base = base_it.next();

    for m in matches {
        while let Some((ref br, style)) = cur_base {
            if br.end <= m.start {
                out.push((br.clone(), style));
                cur_base = base_it.next();
            } else {
                break;
            }
        }
        while let Some((br, style)) = cur_base.take() {
            if br.start >= m.end {
                cur_base = Some((br, style));
                break;
            }
            if br.start < m.start {
                out.push((br.start..m.start, style));
            }
            if br.end > m.end {
                cur_base = Some((m.end..br.end, style));
                break;
            }
            cur_base = base_it.next();
        }
        out.push((m.clone(), match_style));
    }

    if let Some(rem) = cur_base {
        out.push(rem);
    }
    out.extend(base_it);
    out
}

/// Enforce GPUI `StyledText::compute_runs` invariants: strictly sorted, non-empty,
/// disjoint byte ranges aligned on valid UTF-8 character boundaries.
fn sanitize_spans(
    text: &str,
    spans: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut clean = Vec::with_capacity(spans.len());
    let mut cursor = 0usize;
    for (r, style) in spans {
        let start = r.start.max(cursor);
        let end = r.end.min(text.len());
        if start < end && text.is_char_boundary(start) && text.is_char_boundary(end) {
            clean.push((start..end, style));
            cursor = end;
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_valid_gpui_runs(text: &str, spans: &[(Range<usize>, HighlightStyle)]) {
        let mut prev_end = 0usize;
        for (r, _) in spans {
            assert!(r.start < r.end, "empty span {r:?}");
            assert!(r.start >= prev_end, "overlapping span {r:?} < {prev_end}");
            assert!(r.end <= text.len(), "out of bounds {r:?}");
            assert!(text.is_char_boundary(r.start));
            assert!(text.is_char_boundary(r.end));
            prev_end = r.end;
        }
    }

    #[test]
    fn json_xml_form_and_cjk_search_highlights_stay_disjoint_and_char_aligned() {
        let json = "{\n  \"orderId\": \"ord-42\",\n  \"note\": \"中文订单 🚀\",\n  \"amount\": 99.5,\n  \"paid\": true,\n  \"meta\": null\n}";
        let tokens = vec!["ord".to_string(), "订单".to_string(), "99".to_string()];
        let (spans, count) = highlight_code(json, SyntaxKind::Json, &tokens, &theme::DARK);
        assert_eq!(count, 4); // "ord" in orderId, "ord" in ord-42, "订单", "99"
        assert_valid_gpui_runs(json, &spans);

        let xml = "<?xml version=\"1.0\"?><Error><Code>NoSuchKey</Code><Message>Missing key</Message></Error>";
        let (xml_spans, xml_cnt) = highlight_code(
            xml,
            SyntaxKind::Xml,
            &["nosuchkey".to_string()],
            &theme::LIGHT,
        );
        assert_eq!(xml_cnt, 1);
        assert_valid_gpui_runs(xml, &xml_spans);

        let form = "Action = SendMessage\nQueueUrl = http://127.0.0.1:4566/000000000000/jobs\nMessageBody = {\n  \"hello\": \"world\"\n}\n";
        let (form_spans, form_cnt) = highlight_code(
            form,
            SyntaxKind::Form,
            &["sendmessage".to_string(), "world".to_string()],
            &theme::DARK,
        );
        assert_eq!(form_cnt, 2);
        assert_valid_gpui_runs(form, &form_spans);

        // Malformed JSON with backslash immediately before multibyte UTF-8 characters
        // must never slice inside a codepoint.
        let malformed = "{\"note\":\"\\中\",\"emoji\":\"\\🚀\",\"trailing\":\"\\";
        let (bad_spans, bad_cnt) = highlight_code(
            malformed,
            SyntaxKind::Json,
            &["中".to_string()],
            &theme::DARK,
        );
        assert_eq!(bad_cnt, 1);
        assert_valid_gpui_runs(malformed, &bad_spans);
    }
}
