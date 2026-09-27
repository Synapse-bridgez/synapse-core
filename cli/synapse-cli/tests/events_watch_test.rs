use assert_cmd::Command;
use predicates::prelude::*;
use std::net::TcpListener;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener as AsyncTcpListener;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn synapse_cmd() -> Command {
    Command::cargo_bin("synapse").expect("synapse binary must exist")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

// ── CLI shape tests (no network needed) ──────────────────────────────────────

#[test]
fn events_watch_help_is_accepted() {
    synapse_cmd()
        .args(["events", "watch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("token").or(predicate::str::contains("format")));
}

#[test]
fn events_watch_json_flag_appears_in_help() {
    synapse_cmd()
        .args(["events", "watch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("format"));
}

// ── WebSocket lifecycle test ─────────────────────────────────────────────────

/// Spawns a minimal WebSocket server that:
///   1. Completes the HTTP upgrade handshake.
///   2. Sends one `TransactionStatusUpdate` frame.
///   3. Sends a Close frame.
///   4. Waits for the client's Close frame (connection lifecycle check).
///
/// Then verifies that `synapse events watch` prints the event and exits cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn events_watch_receives_event_and_exits_cleanly() {
    let port = free_port();
    let addr = format!("127.0.0.1:{}", port);

    // Spawn mock WS server
    tokio::spawn(async move {
        let listener = AsyncTcpListener::bind(&addr).await.expect("bind");
        if let Ok((mut stream, _)) =
            tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .expect("accept timeout")
        {
            serve_one_event(&mut stream).await;
        }
    });

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    let base_url = format!("http://127.0.0.1:{}", port);

    // Run the CLI with a timeout via `assert_cmd` process timeout
    let assert = synapse_cmd()
        .args([
            "--url",
            &base_url,
            "events",
            "watch",
            "--token",
            "test-token",
        ])
        .timeout(Duration::from_secs(5))
        .assert();

    assert
        .success()
        .stdout(predicate::str::contains("pending").or(predicate::str::contains("tx_id")));
}

/// Send a WebSocket HTTP upgrade response, one text frame, and a close frame.
async fn serve_one_event(stream: &mut tokio::net::TcpStream) {
    // Read HTTP upgrade request
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..n]);

    // Extract Sec-WebSocket-Key
    let key = request
        .lines()
        .find(|l| l.to_lowercase().starts_with("sec-websocket-key:"))
        .and_then(|l| l.splitn(2, ':').nth(1))
        .map(|s| s.trim())
        .unwrap_or("");

    let accept = ws_accept_key(key);

    // Send upgrade response
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept
    );
    let _ = stream.write_all(response.as_bytes()).await;

    // Build and send one text frame with a TransactionStatusUpdate
    let payload = r#"{"transaction_id":"550e8400-e29b-41d4-a716-446655440000","tenant_id":"660e8400-e29b-41d4-a716-446655440000","status":"pending","timestamp":"2026-06-29T10:00:00Z","message":null}"#;
    let frame = ws_text_frame(payload.as_bytes());
    let _ = stream.write_all(&frame).await;

    // Send close frame (opcode 0x8, no payload)
    let close_frame = [0x88u8, 0x00];
    let _ = stream.write_all(&close_frame).await;

    // Drain the client's close frame response
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        let mut drain = [0u8; 128];
        let _ = stream.read(&mut drain).await;
    })
    .await;
}

/// Compute `Sec-WebSocket-Accept` per RFC 6455.
fn ws_accept_key(key: &str) -> String {
    const MAGIC: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let combined = format!("{}{}", key, MAGIC);
    // Use sha1 via a simple implementation to avoid pulling in a new dep.
    // base64(sha1(combined))
    let hash = sha1_bytes(combined.as_bytes());
    base64_encode(&hash)
}

/// Minimal SHA-1 implementation (RFC 3174).
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
        for i in 0..80 {
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
                .wrapping_add(w[i]);
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

/// Minimal base64 encoder.
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

/// Build a WebSocket text frame for the given payload (unmasked, server→client).
fn ws_text_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.push(0x81); // FIN + opcode=text
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
