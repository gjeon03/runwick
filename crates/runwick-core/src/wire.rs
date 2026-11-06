//! Binary framing between the GUI (or another client) and a host.
//!
//! Every frame is `[kind: u8][len: u32 big-endian][payload]`.
//! - kind 1: a JSON-encoded [`Message`] (request, response or event)
//! - kind 2: terminal output for one session: 16-byte session UUID + raw bytes (host -> client)
//! - kind 3: terminal input for one session: 16-byte session UUID + raw bytes (client -> host)
//!
//! Terminal bytes travel unencoded so heavy output does not pay JSON/base64 costs.

use crate::rpc::Message;
use uuid::Uuid;

pub const KIND_MESSAGE: u8 = 1;
pub const KIND_OUTPUT: u8 = 2;
pub const KIND_INPUT: u8 = 3;

/// Upper bound for one frame. Larger terminal writes are split by the sender.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// Terminal chunks are split to this size so one busy session cannot starve others.
pub const MAX_TERMINAL_CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Message(Message),
    Output { session: Uuid, data: Vec<u8> },
    Input { session: Uuid, data: Vec<u8> },
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("frame too large: {0} bytes")]
    TooLarge(usize),
    #[error("unknown frame kind {0}")]
    UnknownKind(u8),
    #[error("malformed frame: {0}")]
    Malformed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl Frame {
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        let (kind, payload) = match self {
            Frame::Message(m) => {
                (KIND_MESSAGE, serde_json::to_vec(m).map_err(|e| WireError::Malformed(e.to_string()))?)
            }
            Frame::Output { session, data } => (KIND_OUTPUT, with_session(session, data)),
            Frame::Input { session, data } => (KIND_INPUT, with_session(session, data)),
        };
        if payload.len() > MAX_FRAME {
            return Err(WireError::TooLarge(payload.len()));
        }
        let mut out = Vec::with_capacity(5 + payload.len());
        out.push(kind);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    fn decode(kind: u8, payload: &[u8]) -> Result<Frame, WireError> {
        match kind {
            KIND_MESSAGE => {
                serde_json::from_slice(payload).map(Frame::Message).map_err(|e| WireError::Malformed(e.to_string()))
            }
            KIND_OUTPUT | KIND_INPUT => {
                if payload.len() < 16 {
                    return Err(WireError::Malformed("short terminal frame".into()));
                }
                let session = Uuid::from_slice(&payload[..16]).map_err(|e| WireError::Malformed(e.to_string()))?;
                let data = payload[16..].to_vec();
                Ok(if kind == KIND_OUTPUT { Frame::Output { session, data } } else { Frame::Input { session, data } })
            }
            other => Err(WireError::UnknownKind(other)),
        }
    }

    /// Split terminal bytes into frames no larger than [`MAX_TERMINAL_CHUNK`].
    pub fn output_chunks(session: Uuid, data: &[u8]) -> impl Iterator<Item = Frame> + '_ {
        data.chunks(MAX_TERMINAL_CHUNK).map(move |c| Frame::Output { session, data: c.to_vec() })
    }

    pub fn input_chunks(session: Uuid, data: &[u8]) -> impl Iterator<Item = Frame> + '_ {
        data.chunks(MAX_TERMINAL_CHUNK).map(move |c| Frame::Input { session, data: c.to_vec() })
    }
}

fn with_session(session: &Uuid, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + data.len());
    v.extend_from_slice(session.as_bytes());
    v.extend_from_slice(data);
    v
}

/// Incremental decoder: push bytes as they arrive, pop complete frames.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete frame, `Ok(None)` when more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, WireError> {
        if self.buf.len() < 5 {
            return Ok(None);
        }
        let kind = self.buf[0];
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if len > MAX_FRAME {
            return Err(WireError::TooLarge(len));
        }
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        let frame = Frame::decode(kind, &self.buf[5..5 + len]);
        self.buf.drain(..5 + len);
        frame.map(Some)
    }
}

/// Blocking helper for std streams (GUI side, stdio bridge).
pub fn write_frame<W: std::io::Write>(w: &mut W, frame: &Frame) -> Result<(), WireError> {
    let bytes = frame.encode()?;
    w.write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::Message;

    #[test]
    fn roundtrip_split_across_pushes() {
        let s = Uuid::new_v4();
        let frames = vec![
            Frame::Message(Message::event("changed", serde_json::json!({"topics": ["projects"]}))),
            Frame::Output { session: s, data: "안녕 \x1b[31mred\x1b[0m".as_bytes().to_vec() },
            Frame::Input { session: s, data: vec![3] },
        ];
        let mut bytes = Vec::new();
        for f in &frames {
            bytes.extend(f.encode().unwrap());
        }
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for chunk in bytes.chunks(3) {
            d.push(chunk);
            while let Some(f) = d.next_frame().unwrap() {
                out.push(f);
            }
        }
        assert_eq!(out, frames);
    }

    #[test]
    fn rejects_oversized_length() {
        let mut d = Decoder::new();
        d.push(&[1, 0xff, 0xff, 0xff, 0xff]);
        assert!(matches!(d.next_frame(), Err(WireError::TooLarge(_))));
    }
}
