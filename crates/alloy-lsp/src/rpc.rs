//! JSON-RPC framing over a byte stream: `Content-Length` headers, then
//! the JSON body.

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// Reads one message. `None` at a clean end of stream.
pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length: Option<usize> = None;

    loop {
        let mut line = String::new();

        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }

        let line = line.trim_end_matches(['\r', '\n']);

        if line.is_empty() {
            break;
        }

        if let Some(rest) = line.strip_prefix("Content-Length:") {
            length = rest.trim().parse().ok();
        }
    }

    let Some(length) = length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message without Content-Length",
        ));
    };

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;

    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Folds `next` into `into` when both are `textDocument/didChange` for
/// one document: the edits of both, in order, at the later version.
/// Returns false, and changes nothing, for any other pair.
pub fn merge_change(into: &mut Value, next: &Value) -> bool {
    let change =
        |m: &Value| m.get("method").and_then(Value::as_str) == Some("textDocument/didChange");
    let uri = |m: &Value| m.pointer("/params/textDocument/uri").cloned();

    if !change(into) || !change(next) || uri(into).is_none() || uri(into) != uri(next) {
        return false;
    }

    let edits = next
        .pointer("/params/contentChanges")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if let Some(list) = into
        .pointer_mut("/params/contentChanges")
        .and_then(Value::as_array_mut)
    {
        list.extend(edits);
    }

    if let (Some(version), Some(slot)) = (
        next.pointer("/params/textDocument/version").cloned(),
        into.pointer_mut("/params/textDocument/version"),
    ) {
        *slot = version;
    }

    true
}

/// Writes one message with its header.
pub fn write_message(writer: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn a_message_round_trips() {
        let value = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "x" });
        let mut buf = Vec::new();
        write_message(&mut buf, &value).unwrap();
        let mut reader = BufReader::new(buf.as_slice());
        assert_eq!(read_message(&mut reader).unwrap(), Some(value));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    fn edit(uri: &str, version: i64, text: &str) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        })
    }

    #[test]
    fn edits_to_one_document_merge_in_order() {
        let mut first = edit("file:///a.aly", 2, "a");
        assert!(merge_change(&mut first, &edit("file:///a.aly", 3, "b")));
        assert_eq!(first["params"]["textDocument"]["version"], 3);
        assert_eq!(
            first["params"]["contentChanges"],
            serde_json::json!([{ "text": "a" }, { "text": "b" }])
        );
    }

    #[test]
    fn other_messages_do_not_merge() {
        let mut first = edit("file:///a.aly", 2, "a");
        let request =
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "textDocument/completion" });
        assert!(!merge_change(&mut first, &edit("file:///b.aly", 3, "b")));
        assert!(!merge_change(&mut first, &request));
        assert_eq!(first, edit("file:///a.aly", 2, "a"));
    }
}
