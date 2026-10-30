//! A minimal WebSocket server endpoint (RFC 6455) for the DevTools bridge: the HTTP upgrade
//! handshake, and text/close/ping frames with fragmentation. One client, on localhost.
//!
//! SHA-1 appears only because the handshake defines `Sec-WebSocket-Accept` with it; it protects
//! nothing here (the bridge's secret is its random path).

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use std::{
    io::{self, ErrorKind, Read, Write},
    net::TcpStream,
};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Largest request head and message we accept.
const MAX_HEAD: usize = 16 * 1024;
const MAX_MESSAGE: usize = 256 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Close,
}

pub struct Socket {
    stream: TcpStream,
    /// Bytes read but not yet parsed into frames.
    buf: Vec<u8>,
    /// A fragmented message being assembled.
    partial: Vec<u8>,
}

/// Completes the handshake on `stream` if the client asks for `path`; otherwise answers 403.
/// Blocks until the request head has arrived.
pub fn accept(mut stream: TcpStream, path: &str) -> io::Result<Socket> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1024];
    let end = loop {
        if let Some(i) = find(&head, b"\r\n\r\n") {
            break i;
        }
        if head.len() > MAX_HEAD {
            return Err(io::Error::new(ErrorKind::InvalidData, "request head too large"));
        }
        let n = stream.read(&mut byte)?;
        if n == 0 {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        head.extend_from_slice(&byte[..n]);
    };
    let text = String::from_utf8_lossy(&head[..end]).into_owned();
    let mut lines = text.split("\r\n");
    let requested = lines.next().and_then(|l| l.strip_prefix("GET ")).and_then(|l| l.split(' ').next()).unwrap_or_default();
    let header = |name: &str| {
        text.split("\r\n").skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
        })
    };
    let upgrade = header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let key = header("sec-websocket-key");
    let (Some(key), true, true) = (key, upgrade, requested == path) else {
        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Err(io::Error::new(ErrorKind::PermissionDenied, "wrong path or not a websocket request"));
    };
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(&key)
    );
    stream.write_all(response.as_bytes())?;
    // Anything after the head is already frame data.
    Ok(Socket { stream, buf: head[end + 4..].to_vec(), partial: Vec::new() })
}

impl Socket {
    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }

    /// The next complete message, `Ok(None)` if the stream's read timeout passed first.
    pub fn read(&mut self) -> io::Result<Option<Message>> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            if let Some(message) = self.parse()? {
                return Ok(Some(message));
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }

    /// Takes complete frames off the buffer until a message is complete.
    fn parse(&mut self) -> io::Result<Option<Message>> {
        loop {
            let Some(Frame { fin, opcode, payload, used }) = frame(&self.buf)? else { return Ok(None) };
            self.buf.drain(..used);
            match opcode {
                // Continuation, text, binary (DevTools only sends text; binary is treated alike).
                0..=2 => {
                    if self.partial.len() + payload.len() > MAX_MESSAGE {
                        return Err(io::Error::new(ErrorKind::InvalidData, "message too large"));
                    }
                    self.partial.extend_from_slice(&payload);
                    if fin {
                        let data = std::mem::take(&mut self.partial);
                        let text = String::from_utf8(data).map_err(|_| io::Error::new(ErrorKind::InvalidData, "invalid utf-8"))?;
                        return Ok(Some(Message::Text(text)));
                    }
                }
                8 => {
                    let _ = self.write_frame(8, &[]);
                    return Ok(Some(Message::Close));
                }
                9 => self.write_frame(10, &payload)?,
                10 => {}
                _ => return Err(io::Error::new(ErrorKind::InvalidData, "unknown opcode")),
            }
        }
    }

    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        self.write_frame(1, text.as_bytes())
    }

    /// One unmasked, final frame (servers never mask).
    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut out = Vec::with_capacity(payload.len() + 10);
        out.push(0x80 | opcode);
        match payload.len() {
            n if n < 126 => out.push(n as u8),
            n if n <= 0xFFFF => {
                out.push(126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(payload);
        self.stream.write_all(&out)
    }
}

struct Frame {
    fin: bool,
    opcode: u8,
    /// Already unmasked.
    payload: Vec<u8>,
    /// Bytes of the buffer the frame took.
    used: usize,
}

/// The frame at the start of `buf`, or `None` if it hasn't fully arrived.
fn frame(buf: &[u8]) -> io::Result<Option<Frame>> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let fin = buf[0] & 0x80 != 0;
    let opcode = buf[0] & 0x0F;
    if buf[0] & 0x70 != 0 {
        return Err(io::Error::new(ErrorKind::InvalidData, "reserved bits set"));
    }
    if buf[1] & 0x80 == 0 {
        return Err(io::Error::new(ErrorKind::InvalidData, "client frames must be masked"));
    }
    let (len, mut at) = match buf[1] & 0x7F {
        126 if buf.len() >= 4 => (u16::from_be_bytes([buf[2], buf[3]]) as usize, 4),
        127 if buf.len() >= 10 => (u64::from_be_bytes(buf[2..10].try_into().unwrap()) as usize, 10),
        126 | 127 => return Ok(None),
        n => (n as usize, 2),
    };
    if len > MAX_MESSAGE {
        return Err(io::Error::new(ErrorKind::InvalidData, "frame too large"));
    }
    if buf.len() < at + 4 + len {
        return Ok(None);
    }
    let mask = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
    at += 4;
    let payload = buf[at..at + len].iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
    Ok(Some(Frame { fin, opcode, payload, used: at + len }))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `Sec-WebSocket-Accept` for a client's key.
fn accept_key(key: &str) -> String {
    B64.encode(sha1(format!("{key}{GUID}").as_bytes()))
}

/// SHA-1 (FIPS 180-4). Only for the handshake above.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (s, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *chunk = word.to_be_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn sha1_vectors() {
        let hex = |b: [u8; 20]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(hex(sha1(long)), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    }

    #[test]
    fn rfc_6455_accept_key() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    /// A client frame: masked, as browsers send them.
    fn client_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [0x12, 0x34, 0x56, 0x78];
        let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
        match payload.len() {
            n if n < 126 => out.push(0x80 | n as u8),
            n if n <= 0xFFFF => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(0x80 | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        out
    }

    #[test]
    fn handshake_and_messages() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            c.write_all(b"GET /devtools/page/abc HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").unwrap();
            // A fragmented message, a ping, a long message, then close.
            c.write_all(&client_frame(false, 1, b"{\"id\":")).unwrap();
            c.write_all(&client_frame(true, 0, b"1}")).unwrap();
            c.write_all(&client_frame(true, 9, b"hi")).unwrap();
            c.write_all(&client_frame(true, 1, &vec![b'x'; 70_000])).unwrap();
            c.write_all(&client_frame(true, 8, b"")).unwrap();
            let mut reply = Vec::new();
            let _ = c.read_to_end(&mut reply);
            reply
        });
        let (stream, _) = listener.accept().unwrap();
        let mut socket = accept(stream, "/devtools/page/abc").unwrap();
        assert_eq!(socket.read().unwrap(), Some(Message::Text("{\"id\":1}".into())));
        assert_eq!(socket.read().unwrap(), Some(Message::Text("x".repeat(70_000))));
        socket.send_text("ok").unwrap();
        assert_eq!(socket.read().unwrap(), Some(Message::Close));
        drop(socket);
        let reply = client.join().unwrap();
        let text = String::from_utf8_lossy(&reply);
        assert!(text.starts_with("HTTP/1.1 101"));
        assert!(text.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
        // Pong for the ping, then our text frame.
        assert!(find(&reply, &[0x8A, 2, b'h', b'i']).is_some());
        assert!(find(&reply, &[0x81, 2, b'o', b'k']).is_some());
    }

    #[test]
    fn wrong_path_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            c.write_all(b"GET /guess HTTP/1.1\r\nUpgrade: websocket\r\nSec-WebSocket-Key: a\r\n\r\n").unwrap();
            let mut reply = String::new();
            let _ = c.read_to_string(&mut reply);
            reply
        });
        let (stream, _) = listener.accept().unwrap();
        assert!(accept(stream, "/devtools/page/abc").is_err());
        assert!(client.join().unwrap().starts_with("HTTP/1.1 403"));
    }
}
