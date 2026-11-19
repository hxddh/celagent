//! Synthetic run files for tests and the micro benchmark.

use std::fmt::Write as _;

pub fn header_line() -> String {
    r#"{"type":"run","version":3,"run":"20261115-094842-1","command":["npm","test"],"cwd":"/w","pid":0,"endpoint":"http://127.0.0.1:1","upstream":"http://127.0.0.1:2","started":"2026-11-15T09:00:00.000+00:00"}"#.to_string()
    + "\n"
}

const SERVICES: [(&str, &str, &str); 5] = [
    ("dynamodb", "GetItem", "arn:aws:dynamodb:*:*:table/orders"),
    ("s3", "PutObject", "arn:aws:s3:::assets/*"),
    ("sqs", "SendMessage", "arn:aws:sqs:*:*:jobs"),
    ("lambda", "Invoke", "arn:aws:lambda:*:*:function:mail"),
    ("kms", "Decrypt", "arn:aws:kms:*:*:key/*"),
];

/// One call line `i` of a synthetic run (`body` goes into the request).
pub fn call_line(i: usize, body: Option<&str>) -> String {
    let (service, op, resource) = SERVICES[i % SERVICES.len()];
    let ms_total = i as u64 * 7;
    let (secs, millis) = (ms_total / 1000, ms_total % 1000);
    let at = format!(
        "2026-11-15T09:{:02}:{:02}.{millis:03}+00:00",
        (secs / 60) % 60,
        secs % 60
    );
    let failing = i % 17 == 3;
    let status = if failing { 400 } else { 200 };
    let mut line = format!(
        r#"{{"type":"call","id":"call-{i}","at":"{at}","method":"POST","path":"/{service}/item-{i}","host":"127.0.0.1:1","service":"{service}","operation":"{op}","status":{status},"ms":{},"authorization":true,"region":"us-east-1","access":[{{"action":"{service}:{op}","resource":"{resource}"{}}}]"#,
        3 + (i % 11) as u64 * 40,
        if i.is_multiple_of(23) {
            r#","new":true"#
        } else {
            ""
        },
    );
    if i.is_multiple_of(23) {
        line.push_str(r#","new":true"#);
    }
    if failing {
        line.push_str(r#","problem":{"code":"ValidationException","message":"One or more parameter values were invalid"}"#);
    }
    let request_body = body.map_or_else(
        || format!(r#"{{"TableName":"orders","Key":{{"id":{{"S":"order-{i}"}}}}}}"#),
        str::to_string,
    );
    let _ = write!(
        line,
        r#","request":{{"headers":[{{"name":"content-type","value":"application/x-amz-json-1.0"}},{{"name":"x-amz-target","value":"DynamoDB_20120810.{op}"}}],"content_type":"application/x-amz-json-1.0","body":{{"text":{},"bytes":{},"truncated":false,"binary":false,"sha256":"00"}}}}"#,
        serde_json::to_string(&request_body).unwrap(),
        request_body.len()
    );
    let _ = write!(
        line,
        r#","response":{{"headers":[{{"name":"content-type","value":"application/json"}},{{"name":"x-amzn-requestid","value":"req-{i}"}}],"content_type":"application/json","body":{{"text":"{{\"Item\":{{\"id\":{{\"S\":\"order-{i}\"}},\"total\":{{\"N\":\"{i}.5\"}}}}}}","bytes":60,"truncated":false,"binary":false,"sha256":"00"}}}}}}"#
    );
    line
}

/// A run file with `calls` calls, optionally ending with a report.
pub fn synthetic_run(calls: usize, body: Option<&str>, end: bool) -> String {
    let mut text = header_line();
    for i in 0..calls {
        text.push_str(&call_line(i, body));
        text.push('\n');
    }
    if end {
        let report = lenslet_core::summary::Report {
            version: 3,
            run: "20261115-094842-1".into(),
            calls,
            ..Default::default()
        };
        let mut value = serde_json::to_value(&report).unwrap();
        value["type"] = "end".into();
        text.push_str(&format!("{value}\n"));
    }
    text
}
