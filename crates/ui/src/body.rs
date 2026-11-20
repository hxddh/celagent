//! Payloads prepared for a line-virtualized viewer: the text to show (decoded
//! or raw), split into lines with their syntax and search highlights, built
//! once per (call, side, mode, query, look) and cached.

use std::{collections::HashMap, ops::Range, sync::Arc};

use gpui::HighlightStyle;

use crate::{
    model::{
        Body, decode_aws_semantic, format_bytes, format_hex_bytes, format_hex_dump, pretty_body,
    },
    syntax::{SyntaxKind, find_search_matches, highlight_code},
    theme::Palette,
};

/// Lines longer than this are broken for display, so a minified megabyte of
/// JSON is many short rows rather than one row that is slow to shape.
const MAX_LINE_BYTES: usize = 2_048;
const CACHE_CAPACITY: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BodyMode {
    Decoded,
    Raw,
}

pub type Spans = Vec<(Range<usize>, HighlightStyle)>;

/// A ready-to-render document.
#[derive(Clone, Debug, Default)]
pub struct BodyDoc {
    pub text: Arc<str>,
    /// Byte range of each display line in `text` (newline excluded).
    pub lines: Vec<Range<usize>>,
    /// Whether a line continues the previous one (a broken long line).
    pub continued: Vec<bool>,
    /// The 1-based source line number of each display line.
    pub numbers: Vec<u32>,
    /// Highlights per line, relative to the line start.
    pub spans: Vec<Spans>,
    /// The longest line, to size horizontal scrolling.
    pub widest: usize,
    pub matches: usize,
    /// Display-line indices containing at least one search match, in ascending order.
    pub match_lines: Vec<usize>,
    /// Multi-line JSON block ranges (`open_line -> close_line`) for code folding.
    pub fold_ranges: HashMap<usize, usize>,
    /// What the decoder recognized, e.g. `DynamoDB` (Decoded mode only).
    pub decoder: Option<&'static str>,
    /// Decoded differs from raw, so the toggle means something.
    pub decodable: bool,
}

impl BodyDoc {
    pub fn line_text(&self, index: usize) -> &str {
        self.lines
            .get(index)
            .map_or("", |range| &self.text[range.clone()])
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Text of lines `range` (inclusive) joined as the user sees them.
    pub fn copy_lines(&self, first: usize, last: usize) -> String {
        let mut out = String::new();
        for index in first..=last.min(self.lines.len().saturating_sub(1)) {
            if index > first && !self.continued[index] {
                out.push('\n');
            }
            out.push_str(self.line_text(index));
        }
        out
    }

    /// Character-accurate slice between `(first_line, first_col)` and `(last_line, last_col)`.
    pub fn copy_selection(
        &self,
        first_line: usize,
        first_col: usize,
        last_line: usize,
        last_col: usize,
    ) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let max_line = self.lines.len() - 1;
        let fl = first_line.min(max_line);
        let ll = last_line.min(max_line);
        let clamp_col = |line_ix: usize, col: usize| -> usize {
            let line = self.line_text(line_ix);
            let mut c = col.min(line.len());
            while c > 0 && !line.is_char_boundary(c) {
                c -= 1;
            }
            c
        };
        if fl == ll {
            let line = self.line_text(fl);
            let s = clamp_col(fl, first_col);
            let e = clamp_col(fl, last_col);
            if s == e {
                return line.to_string();
            }
            return line[s.min(e)..s.max(e)].to_string();
        }
        let mut out = String::new();
        for index in fl..=ll {
            if index > fl && !self.continued[index] {
                out.push('\n');
            }
            let line = self.line_text(index);
            if index == fl {
                out.push_str(&line[clamp_col(index, first_col)..]);
            } else if index == ll {
                out.push_str(&line[..clamp_col(index, last_col)]);
            } else {
                out.push_str(line);
            }
        }
        out
    }

    /// Build a document from `text` highlighted as `kind`.
    pub fn from_text(text: String, kind: SyntaxKind, tokens: &[String], p: &Palette) -> BodyDoc {
        let (spans, matches) = highlight_code(&text, kind, tokens, p);
        let mut doc = BodyDoc {
            matches,
            ..BodyDoc::default()
        };
        // Split into display lines.
        let mut start = 0;
        let bytes = text.as_bytes();
        loop {
            let end = bytes[start..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(text.len(), |rel| start + rel);
            let mut line_end = if end > start && bytes[end - 1] == b'\r' {
                end - 1
            } else {
                end
            };
            let mut piece_start = start;
            let mut first = true;
            while line_end - piece_start > MAX_LINE_BYTES {
                let mut cut = piece_start + MAX_LINE_BYTES;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                doc.lines.push(piece_start..cut);
                doc.continued.push(!first);
                first = false;
                piece_start = cut;
            }
            if line_end < piece_start {
                line_end = piece_start;
            }
            doc.lines.push(piece_start..line_end);
            doc.continued.push(!first);
            if end >= text.len() {
                break;
            }
            start = end + 1;
        }
        let mut number = 0u32;
        doc.numbers = doc
            .continued
            .iter()
            .map(|continued| {
                if !continued {
                    number += 1;
                }
                number
            })
            .collect();
        // Distribute spans over lines (a span may cross lines).
        doc.spans = vec![Vec::new(); doc.lines.len()];
        let mut line = 0;
        for (range, style) in spans {
            while line < doc.lines.len() && doc.lines[line].end <= range.start {
                line += 1;
            }
            let mut cursor = line;
            while cursor < doc.lines.len() && doc.lines[cursor].start < range.end {
                let l = &doc.lines[cursor];
                let s = range.start.max(l.start);
                let e = range.end.min(l.end);
                if s < e {
                    doc.spans[cursor].push((s - l.start..e - l.start, style));
                }
                cursor += 1;
            }
        }
        doc.widest = doc
            .lines
            .iter()
            .enumerate()
            .max_by_key(|(_, range)| range.len())
            .map_or(0, |(index, _)| index);
        if matches > 0 {
            let raw_matches = find_search_matches(&text, tokens);
            let mut line_ix = 0usize;
            for m in raw_matches {
                while line_ix < doc.lines.len() && doc.lines[line_ix].end <= m.start {
                    line_ix += 1;
                }
                let mut cur = line_ix;
                while cur < doc.lines.len() && doc.lines[cur].start < m.end {
                    if doc.match_lines.last().copied() != Some(cur) {
                        doc.match_lines.push(cur);
                    }
                    cur += 1;
                }
            }
        }
        if kind == SyntaxKind::Json && text.len() <= 256 * 1024 {
            doc.fold_ranges = compute_json_folds(&text, &doc.lines);
        }
        doc.text = Arc::from(text);
        doc
    }
}

fn compute_json_folds(text: &str, lines: &[Range<usize>]) -> HashMap<usize, usize> {
    let mut folds = HashMap::new();
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut in_str = false;
    let mut escaped = false;
    for (line_ix, range) in lines.iter().enumerate() {
        for &b in &text.as_bytes()[range.clone()] {
            if in_str {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    in_str = false;
                }
                continue;
            }
            match b {
                b'"' => in_str = true,
                b'{' => stack.push((b'}', line_ix)),
                b'[' => stack.push((b']', line_ix)),
                b'}' | b']' => {
                    if let Some(&(expect, open_line)) = stack.last()
                        && expect == b
                    {
                        stack.pop();
                        if line_ix > open_line {
                            folds.entry(open_line).or_insert(line_ix);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    folds
}

/// Produce a clean unified-style diff between a recorded response body and a replay body.
pub fn diff_text(recorded: &str, replay: &str) -> String {
    if recorded == replay {
        return "--- recorded\n+++ replay\n@@ identical @@\n (Payloads are byte-for-byte identical)"
            .into();
    }
    let a: Vec<&str> = recorded.lines().collect();
    let b: Vec<&str> = replay.lines().collect();
    let mut out = String::from("--- recorded\n+++ replay\n");

    // Trim shared prefix & suffix first so common headers/footers cost O(N).
    let mut prefix = 0usize;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < (a.len() - prefix)
        && suffix < (b.len() - prefix)
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let a_mid = &a[prefix..a.len() - suffix];
    let b_mid = &b[prefix..b.len() - suffix];
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        prefix + 1,
        a_mid.len(),
        prefix + 1,
        b_mid.len()
    ));

    let ctx_before = prefix.saturating_sub(2);
    for line in &a[ctx_before..prefix] {
        out.push(' ');
        out.push_str(line);
        out.push('\n');
    }

    // Bounded LCS over the changed middle window (up to 400x400 lines).
    if a_mid.len() * b_mid.len() <= 160_000 {
        let n = a_mid.len();
        let m = b_mid.len();
        let mut dp = vec![0u16; (n + 1) * (m + 1)];
        let idx = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[idx(i, j)] = if a_mid[i] == b_mid[j] {
                    dp[idx(i + 1, j + 1)].saturating_add(1)
                } else {
                    dp[idx(i + 1, j)].max(dp[idx(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0usize, 0usize);
        while i < n && j < m {
            if a_mid[i] == b_mid[j] {
                out.push(' ');
                out.push_str(a_mid[i]);
                out.push('\n');
                i += 1;
                j += 1;
            } else if dp[idx(i + 1, j)] >= dp[idx(i, j + 1)] {
                out.push('-');
                out.push_str(a_mid[i]);
                out.push('\n');
                i += 1;
            } else {
                out.push('+');
                out.push_str(b_mid[j]);
                out.push('\n');
                j += 1;
            }
        }
        while i < n {
            out.push('-');
            out.push_str(a_mid[i]);
            out.push('\n');
            i += 1;
        }
        while j < m {
            out.push('+');
            out.push_str(b_mid[j]);
            out.push('\n');
            j += 1;
        }
    } else {
        for line in a_mid {
            out.push('-');
            out.push_str(line);
            out.push('\n');
        }
        for line in b_mid {
            out.push('+');
            out.push_str(line);
            out.push('\n');
        }
    }

    let suffix_start = a.len() - suffix;
    for line in &a[suffix_start..(suffix_start + 2).min(a.len())] {
        out.push(' ');
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end_matches('\n').to_string()
}

/// Build the document for a message body.
#[cfg(test)]
pub fn build(
    body: Option<&Body>,
    content_type: &str,
    mode: BodyMode,
    tokens: &[String],
    p: &Palette,
) -> BodyDoc {
    build_with_wire(body, None, content_type, mode, tokens, p)
}

/// Build the document for a message body, including optional lossless wire bytes
/// (from `raw_b64` or sidecar `blob`) so `Raw` mode can show exact `aws-chunked`
/// frames, compressed wire hex, or complete multi-KB binary dumps.
pub fn build_with_wire(
    body: Option<&Body>,
    raw_wire: Option<&[u8]>,
    content_type: &str,
    mode: BodyMode,
    tokens: &[String],
    p: &Palette,
) -> BodyDoc {
    let Some(body) = body else {
        return BodyDoc::default();
    };
    if mode == BodyMode::Raw
        && (body.binary || body.encoding.is_some())
        && let Some(wire) = raw_wire.filter(|w| !w.is_empty())
    {
        if let Ok(utf8_wire) = std::str::from_utf8(wire)
            && !utf8_wire
                .bytes()
                .any(|b| matches!(b, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F))
        {
            let mut doc = BodyDoc::from_text(utf8_wire.to_string(), SyntaxKind::Plain, tokens, p);
            doc.decodable = body.text.is_some();
            return doc;
        }
        let header = match (body.encoding.as_deref(), body.decoded_bytes) {
            (Some(enc), Some(dec)) => format!(
                "# Raw wire frame ({enc}): {} → decoded {}\n",
                format_bytes(wire.len()),
                format_bytes(dec)
            ),
            (Some(enc), None) => {
                format!("# Raw wire frame ({enc}): {}\n", format_bytes(wire.len()))
            }
            _ => String::new(),
        };
        let dump = format!("{header}{}", format_hex_bytes(wire));
        let mut doc = BodyDoc::from_text(dump, SyntaxKind::Hex, tokens, p);
        doc.decodable = body.text.is_some();
        return doc;
    }
    if let Some(text) = body.text.as_deref().filter(|text| !text.is_empty()) {
        let semantic = decode_aws_semantic(text, content_type, body.encoding.as_deref());
        let pretty = if semantic.is_none() {
            pretty_body(text, content_type)
        } else {
            None
        };
        let has_distinct_wire = body.encoding.is_some() && raw_wire.is_some_and(|w| !w.is_empty());
        let decodable = semantic.is_some() || pretty.is_some() || has_distinct_wire;
        let (shown, decoder, is_form) = match (mode, semantic, pretty) {
            (BodyMode::Decoded, Some((label, decoded, is_form)), _) => {
                (decoded, Some(label), is_form)
            }
            (BodyMode::Decoded, None, Some(pretty)) => (pretty, None, false),
            _ => (text.to_string(), None, false),
        };
        let kind = SyntaxKind::detect(&shown, content_type, false, is_form);
        let mut doc = BodyDoc::from_text(shown, kind, tokens, p);
        doc.decoder = decoder;
        doc.decodable = decodable;
        return doc;
    }
    if let Some(wire) = raw_wire.filter(|w| !w.is_empty()) {
        return BodyDoc::from_text(format_hex_bytes(wire), SyntaxKind::Hex, tokens, p);
    }
    if let Some(hex) = body.hex.as_deref().filter(|hex| !hex.is_empty()) {
        return BodyDoc::from_text(format_hex_dump(hex), SyntaxKind::Hex, tokens, p);
    }
    BodyDoc::default()
}

/// Cache key: everything a document depends on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocKey {
    pub call: String,
    pub side: u8,
    pub mode: BodyMode,
    pub query: String,
    pub look: (bool, u8),
}

/// A small most-recently-used cache of built documents.
#[derive(Default)]
pub struct DocCache {
    entries: Vec<(DocKey, Arc<BodyDoc>)>,
}

impl DocCache {
    pub fn get(&mut self, key: &DocKey) -> Option<Arc<BodyDoc>> {
        let index = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(index);
        let doc = entry.1.clone();
        self.entries.insert(0, entry);
        Some(doc)
    }

    pub fn insert(&mut self, key: DocKey, doc: Arc<BodyDoc>) {
        self.entries.retain(|(k, _)| k != &key);
        self.entries.insert(0, (key, doc));
        self.entries.truncate(CACHE_CAPACITY);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    #[test]
    fn splits_lines_breaks_long_ones_and_keeps_spans_on_their_line() {
        let text = "{\"a\": \"x\",\r\n\"b\": 2}".to_string();
        let doc = BodyDoc::from_text(text, SyntaxKind::Json, &["2".into()], &theme::DARK);
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.line_text(0), "{\"a\": \"x\",");
        assert_eq!(doc.line_text(1), "\"b\": 2}");
        assert_eq!(doc.matches, 1);
        for (line, spans) in doc.spans.iter().enumerate() {
            let len = doc.lines[line].len();
            assert!(spans.iter().all(|(r, _)| r.end <= len && r.start < r.end));
        }
        assert_eq!(doc.copy_lines(0, 1), "{\"a\": \"x\",\n\"b\": 2}");

        let long = "é".repeat(3_000);
        let doc = BodyDoc::from_text(long.clone(), SyntaxKind::Plain, &[], &theme::DARK);
        assert!(doc.lines.len() > 1);
        assert!(doc.continued[1]);
        assert_eq!(doc.copy_lines(0, doc.lines.len() - 1), long);
    }

    #[test]
    fn decoded_mode_unwraps_dynamodb_and_raw_keeps_the_wire_text() {
        let body = Body {
            text: Some(r#"{"Item":{"id":{"S":"a"},"n":{"N":"2"}}}"#.into()),
            bytes: 40,
            ..Body::default()
        };
        let decoded = build(
            Some(&body),
            "application/json",
            BodyMode::Decoded,
            &[],
            &theme::DARK,
        );
        assert_eq!(decoded.decoder, Some("DynamoDB Unwrapped"));
        assert!(decoded.text.contains("\"id\": \"a\""));
        let raw = build(
            Some(&body),
            "application/json",
            BodyMode::Raw,
            &[],
            &theme::DARK,
        );
        assert_eq!(&*raw.text, body.text.as_deref().unwrap());
        assert!(raw.decodable);

        let mut cache = DocCache::default();
        let key = DocKey {
            call: "c".into(),
            side: 0,
            mode: BodyMode::Raw,
            query: String::new(),
            look: (true, 0),
        };
        cache.insert(key.clone(), Arc::new(raw));
        assert!(cache.get(&key).is_some());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn json_folds_match_lines_char_selection_and_diff_work() {
        let json = "{\n  \"a\": {\n    \"id\": \"ord-1\"\n  },\n  \"b\": \"ord-2\"\n}";
        let doc = BodyDoc::from_text(
            json.into(),
            SyntaxKind::Json,
            &["ord-".into()],
            &theme::DARK,
        );
        assert_eq!(doc.match_lines, vec![2, 4]);
        assert_eq!(doc.fold_ranges.get(&0), Some(&5));
        assert_eq!(doc.fold_ranges.get(&1), Some(&3));
        assert_eq!(doc.copy_selection(2, 11, 2, 16), "ord-1");

        let diff = diff_text("{\n  \"ok\": false\n}", "{\n  \"ok\": true\n}");
        assert!(diff.contains("-  \"ok\": false"));
        assert!(diff.contains("+  \"ok\": true"));
    }
}
