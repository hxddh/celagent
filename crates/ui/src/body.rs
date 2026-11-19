//! Payloads prepared for a line-virtualized viewer: the text to show (decoded
//! or raw), split into lines with their syntax and search highlights, built
//! once per (call, side, mode, query, look) and cached.

use std::{ops::Range, sync::Arc};

use gpui::HighlightStyle;

use crate::{
    model::{Body, decode_aws_semantic, format_hex_dump, pretty_body},
    syntax::{SyntaxKind, highlight_code},
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
        doc.text = Arc::from(text);
        doc
    }
}

/// Build the document for a message body.
pub fn build(
    body: Option<&Body>,
    content_type: &str,
    mode: BodyMode,
    tokens: &[String],
    p: &Palette,
) -> BodyDoc {
    let Some(body) = body else {
        return BodyDoc::default();
    };
    if let Some(text) = body.text.as_deref().filter(|text| !text.is_empty()) {
        let semantic = decode_aws_semantic(text, content_type, body.encoding.as_deref());
        let pretty = if semantic.is_none() {
            pretty_body(text, content_type)
        } else {
            None
        };
        let decodable = semantic.is_some() || pretty.is_some();
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
}
