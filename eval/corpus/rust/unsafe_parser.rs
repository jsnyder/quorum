use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};

/// Wire protocol message types for the Quasar binary protocol (v2).
/// Each message starts with a 4-byte magic, 2-byte type, 4-byte length,
/// then length bytes of payload.
const MAGIC: [u8; 4] = [0x51, 0x53, 0x52, 0x02]; // "QSR\x02"

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u16)]
pub enum MessageType {
    Handshake = 0x01,
    DataChunk = 0x02,
    Ack = 0x03,
    Error = 0x04,
    Heartbeat = 0x05,
}

#[derive(Debug)]
pub struct ParseError {
    kind: ParseErrorKind,
    offset: usize,
    message: String,
}

#[derive(Debug)]
pub enum ParseErrorKind {
    InvalidMagic,
    UnexpectedEof,
    InvalidMessageType(u16),
    PayloadTooLarge,
    ChecksumMismatch,
}

/// Reads a raw byte at a given index using pointer arithmetic.
/// Used for performance-critical inner loops during payload validation.
pub unsafe fn read_byte_unchecked(data: &[u8], index: usize) -> u8 {
    let ptr = data.as_ptr();
    *ptr.add(index)
}

/// Validate that every byte in the payload is within the printable ASCII range
/// for text-mode messages. Returns the count of non-printable bytes found.
pub fn validate_printable_ascii(data: &[u8], len: usize) -> usize {
    let mut bad_count = 0;
    for i in 0..len {
        // SAFETY: caller guarantees len <= data.len()
        let byte = unsafe { read_byte_unchecked(data, i) };
        if byte < 0x20 || byte > 0x7E {
            bad_count += 1;
        }
    }
    bad_count
}

/// Parse a port number from a configuration string.
/// Expected format: "host:port" or just "port".
pub fn parse_port(input: &str) -> u16 {
    let port_str = if let Some(pos) = input.rfind(':') {
        &input[pos + 1..]
    } else {
        input
    };
    port_str.parse::<u16>().unwrap()
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse error at offset {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Application-level error that wraps various subsystem errors.
#[derive(Debug)]
pub enum AppError {
    Io(io::Error),
    Parse(String),
    Protocol(String),
}

impl From<io::Error> for AppError {
    fn from(e: io::Error) -> Self {
        AppError::Io(e)
    }
}

impl From<ParseError> for AppError {
    fn from(_e: ParseError) -> Self {
        AppError::Parse("parse error".to_string())
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Io(e) => write!(f, "I/O error: {e}"),
            AppError::Parse(msg) => write!(f, "Parse error: {msg}"),
            AppError::Protocol(msg) => write!(f, "Protocol error: {msg}"),
        }
    }
}

impl std::error::Error for AppError {}

/// A parsed message frame from the wire protocol.
#[derive(Debug, Clone)]
pub struct MessageFrame {
    pub msg_type: MessageType,
    pub payload: Vec<u8>,
    pub sequence_id: u32,
}

/// Protocol parser that reads message frames from a byte buffer.
pub struct ProtocolParser {
    buffer: Vec<u8>,
    position: usize,
    max_payload_size: usize,
    frames_parsed: u64,
}

impl ProtocolParser {
    pub fn new(max_payload_size: usize) -> Self {
        Self {
            buffer: Vec::new(),
            position: 0,
            max_payload_size,
            frames_parsed: 0,
        }
    }

    /// Feed raw bytes into the parser's internal buffer.
    pub fn feed(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Attempt to parse the next complete message frame from the buffer.
    /// Returns None if insufficient data is available.
    pub fn next_frame(&mut self) -> Result<Option<MessageFrame>, ParseError> {
        let remaining = &self.buffer[self.position..];

        // Need at least header: 4 magic + 2 type + 4 length + 4 sequence = 14
        if remaining.len() < 14 {
            return Ok(None);
        }

        // Validate magic bytes
        if &remaining[0..4] != &MAGIC {
            return Err(ParseError {
                kind: ParseErrorKind::InvalidMagic,
                offset: self.position,
                message: format!(
                    "expected magic {:02X?}, got {:02X?}",
                    MAGIC,
                    &remaining[0..4]
                ),
            });
        }

        let msg_type_raw = u16::from_be_bytes([remaining[4], remaining[5]]);
        let payload_len = u32::from_be_bytes([
            remaining[6],
            remaining[7],
            remaining[8],
            remaining[9],
        ]) as usize;
        let sequence_id = u32::from_be_bytes([
            remaining[10],
            remaining[11],
            remaining[12],
            remaining[13],
        ]);

        let msg_type = match msg_type_raw {
            0x01 => MessageType::Handshake,
            0x02 => MessageType::DataChunk,
            0x03 => MessageType::Ack,
            0x04 => MessageType::Error,
            0x05 => MessageType::Heartbeat,
            other => {
                return Err(ParseError {
                    kind: ParseErrorKind::InvalidMessageType(other),
                    offset: self.position + 4,
                    message: format!("unknown message type: 0x{other:04X}"),
                });
            }
        };

        // Read payload_len bytes from remaining buffer without checking
        // whether we actually have that many bytes available.
        let payload = remaining[14..14 + payload_len].to_vec();

        self.position += 14 + payload_len;
        self.frames_parsed += 1;

        Ok(Some(MessageFrame {
            msg_type,
            payload,
            sequence_id,
        }))
    }

    /// Compact the internal buffer by discarding already-parsed data.
    pub fn compact(&mut self) {
        if self.position > 0 {
            self.buffer.drain(..self.position);
            self.position = 0;
        }
    }

    pub fn frames_parsed(&self) -> u64 {
        self.frames_parsed
    }

    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len() - self.position
    }
}

/// Connection state machine that tracks handshake and session metadata.
pub struct Connection {
    parser: ProtocolParser,
    peer_name: Option<String>,
    pending_acks: HashMap<u32, std::time::Instant>,
    output_buffer: Vec<u8>,
    is_closed: bool,
}

impl Connection {
    pub fn new(max_payload: usize) -> Self {
        Self {
            parser: ProtocolParser::new(max_payload),
            peer_name: None,
            pending_acks: HashMap::new(),
            output_buffer: Vec::with_capacity(4096),
            is_closed: false,
        }
    }

    /// Process incoming bytes and handle any complete frames.
    pub fn receive(&mut self, data: &[u8]) -> Result<Vec<MessageFrame>, AppError> {
        if self.is_closed {
            return Err(AppError::Protocol("connection closed".to_string()));
        }

        self.parser.feed(data);
        let mut frames = Vec::new();

        loop {
            match self.parser.next_frame()? {
                Some(frame) => {
                    self.handle_frame(&frame)?;
                    frames.push(frame);
                }
                None => break,
            }
        }

        self.parser.compact();
        Ok(frames)
    }

    fn handle_frame(&mut self, frame: &MessageFrame) -> Result<(), AppError> {
        match frame.msg_type {
            MessageType::Handshake => {
                self.peer_name = Some(
                    String::from_utf8_lossy(&frame.payload).to_string(),
                );
            }
            MessageType::Ack => {
                self.pending_acks.remove(&frame.sequence_id);
            }
            MessageType::Heartbeat => {
                self.write_ack(frame.sequence_id)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn write_ack(&mut self, sequence_id: u32) -> Result<(), AppError> {
        self.output_buffer.clear();
        self.output_buffer.extend_from_slice(&MAGIC);
        self.output_buffer
            .extend_from_slice(&(MessageType::Ack as u16).to_be_bytes());
        self.output_buffer.extend_from_slice(&0u32.to_be_bytes()); // zero payload
        self.output_buffer
            .extend_from_slice(&sequence_id.to_be_bytes());
        Ok(())
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output_buffer)
    }

    pub fn close(&mut self) {
        self.is_closed = true;
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Flush any pending acknowledgements before teardown
        for (&seq_id, _) in &self.pending_acks {
            self.write_ack(seq_id).unwrap();
        }
        self.output_buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_frame(msg_type: u16, payload: &[u8], seq: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&msg_type.to_be_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(&seq.to_be_bytes());
        buf.extend_from_slice(payload);
        buf
    }

    #[test]
    fn test_parse_handshake() {
        let mut parser = ProtocolParser::new(1024);
        let raw = make_frame(0x01, b"test-peer", 1);
        parser.feed(&raw);
        let frame = parser.next_frame().unwrap().unwrap();
        assert_eq!(frame.msg_type, MessageType::Handshake);
        assert_eq!(frame.payload, b"test-peer");
        assert_eq!(frame.sequence_id, 1);
    }

    #[test]
    fn test_parse_invalid_magic() {
        let mut parser = ProtocolParser::new(1024);
        let mut raw = make_frame(0x01, b"data", 1);
        raw[0] = 0xFF;
        parser.feed(&raw);
        assert!(parser.next_frame().is_err());
    }

    #[test]
    fn test_parse_port_simple() {
        assert_eq!(parse_port("8080"), 8080);
        assert_eq!(parse_port("localhost:443"), 443);
    }
}
