//! Minimal, dependency-free RFC 6455 handshake + frame helpers.
//!
//! Used by tests that need to hand-craft raw WebSocket frames — e.g. a
//! genuinely fragmented (multi-frame) message — which none of our real
//! dependencies (axum's `ws` feature, tokio-tungstenite) will ever produce
//! on the *sending* side, since neither auto-fragments outgoing writes.

/// WebSocket opcodes (RFC 6455 §11.8).
pub const OPCODE_CONTINUATION: u8 = 0x0;
pub const OPCODE_TEXT: u8 = 0x1;
#[allow(dead_code)]
pub const OPCODE_BINARY: u8 = 0x2;
pub const OPCODE_CLOSE: u8 = 0x8;
#[allow(dead_code)]
pub const OPCODE_PING: u8 = 0x9;
#[allow(dead_code)]
pub const OPCODE_PONG: u8 = 0xA;

/// Compute `Sec-WebSocket-Accept` per RFC 6455 §1.3.
pub fn ws_accept_key(key: &str) -> String {
    const MAGIC: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let combined = format!("{}{}", key, MAGIC);
    let hash = sha1_bytes(combined.as_bytes());
    base64_encode(&hash)
}

/// Extract the `Sec-WebSocket-Key` header value from a raw HTTP upgrade request.
pub fn extract_ws_key(request: &str) -> &str {
    request
        .lines()
        .find(|l| l.to_lowercase().starts_with("sec-websocket-key:"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, s)| s.trim())
        .unwrap_or("")
}

/// Build the HTTP 101 upgrade response for a given raw request.
pub fn upgrade_response(request: &str) -> String {
    let accept = ws_accept_key(extract_ws_key(request));
    format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept
    )
}

/// Build a single unmasked (server→client) WebSocket frame.
///
/// `fin = false` marks a non-final fragment — the next frame in the message
/// must use `opcode = OPCODE_CONTINUATION`.
pub fn ws_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.push((if fin { 0x80 } else { 0x00 }) | (opcode & 0x0F));
    let len = payload.len();
    if len < 126 {
        frame.push(len as u8);
    } else if len < 65536 {
        frame.push(126);
        frame.push((len >> 8) as u8);
        frame.push((len & 0xff) as u8);
    } else {
        frame.push(127);
        for b in (len as u64).to_be_bytes() {
            frame.push(b);
        }
    }
    frame.extend_from_slice(payload);
    frame
}

/// Fixed Sec-WebSocket-Key from the RFC 6455 §1.2 example — any 16
/// base64-encoded bytes are valid; a fixed value keeps tests deterministic.
pub const SAMPLE_WS_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

/// Build a raw HTTP/1.1 WebSocket upgrade request for `path` against `host`.
pub fn upgrade_request(host: &str, path: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n",
        path = path,
        host = host,
        key = SAMPLE_WS_KEY,
    )
}

/// Read a single unmasked (server→client) frame: `(fin, opcode, payload)`.
///
/// Only handles the payload-length encodings the server actually sends
/// (no masking, since RFC 6455 forbids masked server→client frames).
pub async fn read_frame<S>(stream: &mut S) -> std::io::Result<(bool, u8, Vec<u8>)>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut header = [0u8; 2];
    stream.read_exact(&mut header).await?;
    let fin = header[0] & 0x80 != 0;
    let opcode = header[0] & 0x0F;
    let masked = header[1] & 0x80 != 0;
    let mut len = (header[1] & 0x7F) as u64;

    if len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext).await?;
        len = u16::from_be_bytes(ext) as u64;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext).await?;
        len = u64::from_be_bytes(ext);
    }

    let mask_key = if masked {
        let mut m = [0u8; 4];
        stream.read_exact(&mut m).await?;
        Some(m)
    } else {
        None
    };

    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;
    if let Some(m) = mask_key {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }

    Ok((fin, opcode, payload))
}

/// Minimal SHA-1 implementation (RFC 3174) — avoids pulling in a new dependency
/// just for the WebSocket handshake's Sec-WebSocket-Accept computation.
fn sha1_bytes(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    for b in bit_len.to_be_bytes() {
        msg.push(b);
    }

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, &hi) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&hi.to_be_bytes());
    }
    out
}

/// Minimal base64 encoder (standard alphabet, with padding).
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = if chunk.len() > 1 {
            chunk[1] as usize
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as usize
        } else {
            0
        };
        out.push(ALPHABET[b0 >> 2] as char);
        out.push(ALPHABET[((b0 & 3) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((b1 & 0xf) << 2) | (b2 >> 6)] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[b2 & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}
