# Lenslet trace

Each `lenslet run` / `lenslet proxy` session appends one JSON object per call to
its own file, `~/.lenslet/runs/YYYYMMDD-HHMMSS-PID.jsonl` (or `--trace PATH`).
The window follows the newest run, or `LENSLET_TRACE` when set. The schema is
additive: 2.0 adds fields, removes none, and readers ignore unknown fields.

## Top level

| field | type | meaning |
| --- | --- | --- |
| `id` | string | unique call id (hex nanoseconds + sequence) |
| `at` | string | RFC3339 request-arrival time |
| `method` | string | HTTP method |
| `path` | string | path and query exactly as sent |
| `host` | string | original Host header |
| `service` | string | SigV4 scope service (header or presigned URL); `s3` for SigV2; else `aws` |
| `region` | string? | SigV4 scope region (2.0) |
| `operation` | string | protocol-derived operation, S3 REST shape, or HTTP method |
| `status` | number | HTTP status; `0` while pending |
| `ms` | number | complete exchange duration in milliseconds |
| `authorization` | bool | an Authorization header was present |
| `pending` | bool? | `true` on a line written for a call still running after 1 s (2.0) |
| `error` | string? | proxy-side failure, e.g. the emulator was unreachable |
| `problem` | object? | why the call failed, for status ≥ 400 or a proxy failure (2.0) |
| `request` | object | captured request |
| `response` | object | captured response |

### Pending lines

A call that has not finished one second after it arrived gets a line with
`"pending": true`, `"status": 0` and no bodies. When it finishes, a second line
with the **same `id`** is written. Readers keep the last line per `id`. Fast
calls are written once.

### `problem`

```json
{"code": "NoSuchKey", "message": "The specified key does not exist.",
 "hint": "The resource does not exist in the emulator. Was it created in this run, in the same region?"}
```

- `code`: from a JSON body (`__type`, `code`), an XML body (`<Code>`, including
  Query `<Error>`), or the `x-amzn-ErrorType` / `x-amzn-query-error` header;
  otherwise the HTTP reason (`NotFound`). A proxy failure is
  `EmulatorUnreachable`.
- `message`: the error message, when one was sent.
- `hint`: a one-line likely cause, only for codes with an obvious one.

## Message

```json
{
  "headers": [{"name": "content-type", "value": "application/json"}],
  "content_type": "application/json",
  "body": {"text": "…", "bytes": 39, "truncated": false, "binary": false, "sha256": "…"}
}
```

- Headers are raw and duplicate-preserving. There is no redaction.
- `body` is absent when the body was empty.
- `text` is absent for binary/non-text bodies (including content-encoded
  bodies). `binary` is true for those.
- `bytes` is the full body length; `sha256` covers the full body.
- `truncated` means `text` holds only the first 64 KiB.

## Operation naming

1. `x-amz-target` last segment
2. `x-id` query parameter
3. `Action=` in an `application/x-www-form-urlencoded` request body
4. S3 only: the REST shape — method, bucket/key, and query subresource
   (`GET /b/k` → `GetObject`, `GET /b?list-type=2` → `ListObjectsV2`,
   `PUT /b/k?tagging` → `PutObjectTagging`, multipart, …)
5. HTTP method

## Run sidecar

Next to each trace, `X.run.json` describes the session:

```json
{"kind": "run", "command": ["npm", "test"], "pid": 4242,
 "endpoint": "http://127.0.0.1:43761", "upstream": "https://127.0.0.1:9298",
 "started": "2026-11-15T05:46:28Z", "ended": "2026-11-15T05:46:29Z", "exit_code": 0}
```

`kind` is `run` or `proxy`; `ended` and `exit_code` appear when the session
ends. Only the newest 20 runs are kept.

## Files

- `~/.lenslet/runs/`: traces and sidecars, owner-only.
- `~/.lenslet/proxy-cert.pem` / `proxy-key.pem`: self-signed TLS identity for
  `https://` clients (`127.0.0.1`, `localhost`, `::1`). Keep the key private.
- `~/.aws/emulator/PORT/`: the emulator's credentials, config and CA
  (`ca.pem`), read by the proxy and `lenslet run`.
