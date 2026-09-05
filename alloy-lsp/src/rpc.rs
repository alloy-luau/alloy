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
}
