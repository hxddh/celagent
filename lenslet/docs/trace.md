# Lenslet trace

The proxy appends one JSON object per proxied call to
`~/.lenslet/trace.jsonl` (or `--trace PATH`). The UI tails this file. The schema
is intentionally small and additive; unknown fields are ignored.

## Top level

| field | type | meaning |
| --- | --- | --- |
| `id` | string | unique call id (hex nanoseconds + sequence) |
| `at` | string | RFC3339 request-arrival time |
| `method` | string | HTTP method |
| `path` | string | path and query exactly as sent |
| `host` | string | original Host header |
| `service` | string | SigV4 credential-scope service, or `aws` |
| `operation` | string | protocol-derived operation or HTTP method |
| `status` | number | HTTP status |
| `ms` | number | complete exchange duration in milliseconds |
| `authorization` | bool | an Authorization header was present |
| `error` | string? | proxy/upstream error, when the exchange failed |
| `request` | object? | captured request |
| `response` | object? | captured response |

## Message

```json
{
  "headers": [{"name": "content-type", "value": "application/json"}],
  "content_type": "application/json",
  "body": {
    "text": "…",
    "bytes": 39,
    "truncated": false,
    "binary": false,
    "sha256": "…"
  }
}
```

- Headers are raw and duplicate-preserving. There is no redaction.
- `body` is absent when the body was empty.
- `text` is absent for binary/non-text bodies (including content-encoded
  bodies). `binary` is true for those.
- `bytes` is the full body length; `sha256` covers the full body.
- `truncated` means the text capture holds only the first 64 KiB.
- Request and response capture are independent.

## Operation naming

1. `x-amz-target` last segment
2. `x-id` query value
3. `Action=` in an `application/x-www-form-urlencoded` request body
4. HTTP method (plus `PUT /bucket/` → `CreateBucket`)

The service always comes from the SigV4 credential scope. Unknown protocols
keep the method fallback.

## Files

- `~/.lenslet/proxy-cert.pem` / `proxy-key.pem`: self-signed TLS material for
  SDK clients. Trust the certificate; keep the key private.
- `~/.lenslet/trace.jsonl`: raw local trace, mode `0600`.
- `~/.aws/emulator/9298/ca.pem`: emulator upstream CA read by the proxy.
