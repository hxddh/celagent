//! A tiny `text/event-stream` client on `std::net::TcpStream` for the proxy's
//! live event stream (`GET {endpoint}/_lenslet/events`). HTTP/1.1 only, with
//! `Transfer-Encoding: chunked` support; each event's `data` is one exact
//! run-file line. Blocking: run it on its own thread.

use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

pub const EVENTS_PATH: &str = "/_lenslet/events";
const CONNECT_TIMEOUT: Duration = Duration::from_millis(600);
/// How often a blocked read wakes up to check whether to stop.
const READ_TICK: Duration = Duration::from_millis(500);
/// The proxy sends a keepalive every 15 s; silence for this long is a drop.
const STALE_AFTER: Duration = Duration::from_secs(45);
const MAX_HEADER_BYTES: usize = 64 * 1024;
/// One event never exceeds a run-file line; bound a runaway stream.
const MAX_EVENT_BYTES: usize = 64 << 20;

/// How a subscription ended.
#[derive(Debug, PartialEq, Eq)]
pub enum SseEnd {
    /// The server closed the stream cleanly.
    Closed,
    /// No event stream there (refused, timed out, not 200, wrong type).
    Unavailable(String),
    /// Connected, then the stream broke or went silent.
    Dropped(String),
    /// The caller asked to stop.
    Stopped,
}

/// `http://127.0.0.1:43761` (optionally with a base path) → address, host header, path.
fn parse_url(endpoint: &str) -> Option<(String, String, String)> {
    let rest = endpoint.trim().strip_prefix("http://")?;
    let (authority, base) = match rest.find('/') {
        Some(ix) => (&rest[..ix], rest[ix..].trim_end_matches('/')),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return None;
    }
    let address = if authority
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
    {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };
    Some((
        address,
        authority.to_string(),
        format!("{base}{EVENTS_PATH}"),
    ))
}

/// Subscribe to `endpoint`'s event stream; `on_event` gets each event's data
/// and returns `false` to stop. `should_stop` is polled while waiting.
pub fn subscribe(
    endpoint: &str,
    should_stop: impl Fn() -> bool,
    mut on_event: impl FnMut(&str) -> bool,
) -> SseEnd {
    let Some((address, host, path)) = parse_url(endpoint) else {
        return SseEnd::Unavailable(format!("not an http endpoint: {endpoint}"));
    };
    let Some(addr) = address.to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        return SseEnd::Unavailable(format!("cannot resolve {address}"));
    };
    let mut stream = match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) => return SseEnd::Unavailable(error.to_string()),
    };
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(READ_TICK));
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: text/event-stream\r\nCache-Control: no-cache\r\nUser-Agent: lenslet-ui\r\n\r\n"
    );
    if let Err(error) = stream.write_all(request.as_bytes()) {
        return SseEnd::Unavailable(error.to_string());
    }

    let mut reader = Reader {
        stream,
        last_data: Instant::now(),
    };
    // Response head.
    let mut head: Vec<u8> = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let (chunked, mut body_start) = loop {
        match reader.read(&mut buf, &should_stop) {
            ReadOutcome::Data(n) => head.extend_from_slice(&buf[..n]),
            ReadOutcome::Eof => return SseEnd::Unavailable("closed before a response".into()),
            ReadOutcome::Stop => return SseEnd::Stopped,
            ReadOutcome::Error(error) => return SseEnd::Unavailable(error),
        }
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut response = httparse::Response::new(&mut headers);
        match response.parse(&head) {
            Ok(httparse::Status::Complete(len)) => {
                let status = response.code.unwrap_or(0);
                if status != 200 {
                    return SseEnd::Unavailable(format!("HTTP {status}"));
                }
                let header = |name: &str| {
                    response
                        .headers
                        .iter()
                        .find(|h| h.name.eq_ignore_ascii_case(name))
                        .map(|h| String::from_utf8_lossy(h.value).to_ascii_lowercase())
                };
                if !header("content-type").is_some_and(|ct| ct.contains("text/event-stream")) {
                    return SseEnd::Unavailable("not an event stream".into());
                }
                let chunked = header("transfer-encoding").is_some_and(|te| te.contains("chunked"));
                break (chunked, head.split_off(len));
            }
            Ok(httparse::Status::Partial) if head.len() < MAX_HEADER_BYTES => continue,
            _ => return SseEnd::Unavailable("bad response head".into()),
        }
    };

    let mut chunks = ChunkedDecoder::default();
    let mut events = EventParser::default();
    loop {
        let decoded = if chunked {
            let mut out = Vec::new();
            if let Err(error) = chunks.feed(&body_start, &mut out) {
                return SseEnd::Dropped(error);
            }
            out
        } else {
            std::mem::take(&mut body_start)
        };
        body_start.clear();
        let mut keep_going = true;
        let pushed = events.push(&decoded, |data| {
            if keep_going {
                keep_going = on_event(data);
            }
        });
        if let Err(error) = pushed {
            return SseEnd::Dropped(error);
        }
        if !keep_going {
            return SseEnd::Stopped;
        }
        if chunked && chunks.done() {
            return SseEnd::Closed;
        }
        match reader.read(&mut buf, &should_stop) {
            ReadOutcome::Data(n) => body_start.extend_from_slice(&buf[..n]),
            ReadOutcome::Eof => {
                return if chunked && !chunks.done() {
                    SseEnd::Dropped("stream ended mid-chunk".into())
                } else {
                    SseEnd::Closed
                };
            }
            ReadOutcome::Stop => return SseEnd::Stopped,
            ReadOutcome::Error(error) => return SseEnd::Dropped(error),
        }
    }
}

enum ReadOutcome {
    Data(usize),
    Eof,
    Stop,
    Error(String),
}

struct Reader {
    stream: TcpStream,
    last_data: Instant,
}

impl Reader {
    fn read(&mut self, buf: &mut [u8], should_stop: &impl Fn() -> bool) -> ReadOutcome {
        loop {
            if should_stop() {
                return ReadOutcome::Stop;
            }
            match self.stream.read(buf) {
                Ok(0) => return ReadOutcome::Eof,
                Ok(n) => {
                    self.last_data = Instant::now();
                    return ReadOutcome::Data(n);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    if self.last_data.elapsed() > STALE_AFTER {
                        return ReadOutcome::Error("event stream went silent".into());
                    }
                }
                Err(error) => return ReadOutcome::Error(error.to_string()),
            }
        }
    }
}

/// Incremental `Transfer-Encoding: chunked` decoder.
#[derive(Default)]
pub struct ChunkedDecoder {
    state: ChunkState,
    line: Vec<u8>,
}

#[derive(Default, PartialEq, Eq)]
enum ChunkState {
    #[default]
    Size,
    Data(usize),
    DataEnd,
    Trailer,
    Done,
}

impl ChunkedDecoder {
    pub fn done(&self) -> bool {
        self.state == ChunkState::Done
    }

    pub fn feed(&mut self, mut input: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
        while !input.is_empty() {
            match self.state {
                ChunkState::Size | ChunkState::DataEnd | ChunkState::Trailer => {
                    let Some(nl) = input.iter().position(|b| *b == b'\n') else {
                        self.line.extend_from_slice(input);
                        if self.line.len() > 4096 {
                            return Err("chunk header too long".into());
                        }
                        return Ok(());
                    };
                    self.line.extend_from_slice(&input[..nl]);
                    input = &input[nl + 1..];
                    let line = std::mem::take(&mut self.line);
                    let line = line.strip_suffix(b"\r").unwrap_or(&line);
                    match self.state {
                        ChunkState::Size => {
                            let text = String::from_utf8_lossy(line);
                            let hex = text.split(';').next().unwrap_or("").trim();
                            let size = usize::from_str_radix(hex, 16)
                                .map_err(|_| format!("bad chunk size `{hex}`"))?;
                            self.state = if size == 0 {
                                ChunkState::Trailer
                            } else {
                                ChunkState::Data(size)
                            };
                        }
                        ChunkState::DataEnd => {
                            if !line.is_empty() {
                                return Err("missing CRLF after chunk".into());
                            }
                            self.state = ChunkState::Size;
                        }
                        _ => {
                            if line.is_empty() {
                                self.state = ChunkState::Done;
                            }
                        }
                    }
                }
                ChunkState::Data(remaining) => {
                    let take = remaining.min(input.len());
                    out.extend_from_slice(&input[..take]);
                    input = &input[take..];
                    self.state = if take == remaining {
                        ChunkState::DataEnd
                    } else {
                        ChunkState::Data(remaining - take)
                    };
                }
                ChunkState::Done => return Ok(()),
            }
        }
        Ok(())
    }
}

/// Incremental `text/event-stream` parser: `data:` lines joined with `\n`,
/// dispatched on a blank line; comments and other fields ignored. Bytes are
/// decoded per complete line, so a UTF-8 character split across reads is safe.
#[derive(Default)]
pub struct EventParser {
    pending: Vec<u8>,
    data: Option<String>,
}

impl EventParser {
    pub fn push(&mut self, bytes: &[u8], mut dispatch: impl FnMut(&str)) -> Result<(), String> {
        self.pending.extend_from_slice(bytes);
        let mut start = 0;
        while let Some(rel) = self.pending[start..].iter().position(|b| *b == b'\n') {
            let end = start + rel;
            let raw = &self.pending[start..end];
            let line = raw.strip_suffix(b"\r").unwrap_or(raw);
            if line.is_empty() {
                if let Some(data) = self.data.take() {
                    dispatch(&data);
                }
            } else if line[0] != b':' {
                let text = String::from_utf8_lossy(line);
                let (field, value) = text.split_once(':').unwrap_or((&text, ""));
                if field == "data" {
                    let value = value.strip_prefix(' ').unwrap_or(value);
                    match self.data.as_mut() {
                        Some(data) => {
                            data.push('\n');
                            data.push_str(value);
                        }
                        None => self.data = Some(value.to_string()),
                    }
                    if self
                        .data
                        .as_ref()
                        .is_some_and(|d| d.len() > MAX_EVENT_BYTES)
                    {
                        return Err("event too large".into());
                    }
                }
            }
            start = end + 1;
        }
        self.pending.drain(..start);
        if self.pending.len() > MAX_EVENT_BYTES {
            return Err("event line too large".into());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    /// A fake proxy: answers `GET /_lenslet/events` with `events` as chunked
    /// SSE, cut into awkward pieces, then closes. Returns its endpoint.
    pub(crate) fn fake_sse_server(events: Vec<String>, chunked: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
            }
            if !request_line.starts_with("GET /_lenslet/events ") {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                return;
            }
            let mut body = String::from(": keepalive\n\n");
            for event in &events {
                body.push_str(&format!("data: {event}\n\n"));
            }
            body.push_str(": keepalive\r\n\r\n");
            let head = if chunked {
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n"
            } else {
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
            };
            stream.write_all(head.as_bytes()).unwrap();
            // Odd sizes split lines, CRLFs and UTF-8 characters.
            for piece in body.as_bytes().chunks(7) {
                if chunked {
                    stream
                        .write_all(format!("{:x};ext=1\r\n", piece.len()).as_bytes())
                        .unwrap();
                    stream.write_all(piece).unwrap();
                    stream.write_all(b"\r\n").unwrap();
                } else {
                    stream.write_all(piece).unwrap();
                }
                stream.flush().unwrap();
            }
            if chunked {
                stream.write_all(b"0\r\n\r\n").unwrap();
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    fn receives_every_event_over_chunked_and_plain_streams() {
        let events = vec![
            r#"{"type":"run","run":"r"}"#.to_string(),
            r#"{"type":"call","id":"é-1","note":"ünïcode ✓"}"#.to_string(),
            r#"{"type":"end"}"#.to_string(),
        ];
        for chunked in [true, false] {
            let endpoint = fake_sse_server(events.clone(), chunked);
            let mut got = Vec::new();
            let end = subscribe(
                &endpoint,
                || false,
                |data| {
                    got.push(data.to_string());
                    true
                },
            );
            assert_eq!(end, SseEnd::Closed, "chunked={chunked}");
            assert_eq!(got, events, "chunked={chunked}");
        }
    }

    #[test]
    fn reports_a_missing_endpoint_as_unavailable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let end = subscribe(&format!("http://127.0.0.1:{port}"), || false, |_| true);
        assert!(matches!(end, SseEnd::Unavailable(_)), "{end:?}");

        // An older proxy answers, but not with an event stream.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        });
        let end = subscribe(&format!("http://127.0.0.1:{port}"), || false, |_| true);
        assert_eq!(end, SseEnd::Unavailable("HTTP 404".into()));
    }

    #[test]
    fn stops_when_asked_while_the_stream_is_quiet() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            );
            std::thread::sleep(Duration::from_secs(5));
        });
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let end = subscribe(
            &format!("http://127.0.0.1:{port}"),
            || stop.load(Ordering::SeqCst),
            |_| true,
        );
        assert_eq!(end, SseEnd::Stopped);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn parses_multi_line_data_and_comments() {
        let mut parser = EventParser::default();
        let mut got = Vec::new();
        parser
            .push(b"data: a\ndata:b\n: c\nid: 1\n\n\ndata: x", |d| {
                got.push(d.to_string())
            })
            .unwrap();
        assert_eq!(got, ["a\nb"]);
        parser.push(b"\n\n", |d| got.push(d.to_string())).unwrap();
        assert_eq!(got, ["a\nb", "x"]);
        assert_eq!(
            parse_url("http://127.0.0.1:43761/").unwrap(),
            (
                "127.0.0.1:43761".into(),
                "127.0.0.1:43761".into(),
                "/_lenslet/events".into()
            )
        );
    }
}
