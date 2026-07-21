//! End-to-end coverage for `synapse events watch` against the *real*
//! synapse-core server (not the hand-rolled mock in `events_watch_test.rs`).
//!
//! This boots the actual axum app (`synapse_core::create_app`) with a real
//! Postgres-backed `AppState`, then drives the compiled `synapse` binary as
//! a subprocess against its `/ws` endpoint — exercising the real
//! `tokio-tungstenite` client/server stack end to end. This is the kind of
//! coverage that would have caught the tungstenite 0.20/0.21 version split
//! this suite fixes: the unit tests on both sides mock the protocol, so a
//! wire-format or keepalive regression across a tungstenite major would
//! otherwise pass silently.
//!
//! The two tests that boot the real server (`spawn_real_server`) require a
//! reachable Postgres (see `DATABASE_URL` below) and Redis at
//! `redis://localhost:6379` — both are provided by
//! `.github/workflows/rust.yml`'s `integration-tests` job, matching the
//! convention already used by `tests/multi_tenant_test.rs` and
//! `tests/websocket_test.rs` in the root crate. Those two are `#[ignore]`d
//! by default; run explicitly with `cargo test -- --ignored`. The third
//! (fragmentation + close handling) needs no external services and runs as
//! part of the normal test suite.

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::Duration;

use chrono::Utc;
use synapse_core::handlers::ws::TransactionStatusUpdate;
use synapse_core::{create_app, AppState};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

// ── Server-side timing constants ────────────────────────────────────────────
//
// Mirror the private HEARTBEAT_INTERVAL / PONG_TIMEOUT constants in
// src/handlers/ws.rs (30s / 10s). Kept independent here (rather than made
// pub and imported) since a test depending on production timing constants
// would need to change in lockstep anyway; a comment is enough to keep them
// honest.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

// ── Real server harness ─────────────────────────────────────────────────────

/// Boots the actual synapse-core axum app on an ephemeral port, backed by a
/// real database connection. Returns the CLI-facing `http://` base URL and
/// the `AppState`, so tests can push broadcast updates exactly as
/// production code does via `state.tx_broadcast`.
async fn spawn_real_server() -> (String, AppState) {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    let state = AppState::test_new(&database_url).await;
    let app = create_app(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let std_listener = listener.into_std().expect("into_std");

    tokio::spawn(async move {
        let _ = axum::Server::from_tcp(std_listener)
            .expect("axum::Server::from_tcp")
            .serve(app.into_make_service())
            .await;
    });

    (format!("http://{}", addr), state)
}

// ── Real CLI harness ─────────────────────────────────────────────────────────

/// Spawns the real `synapse events watch` process against `base_url` and
/// streams its stdout into an accumulating buffer so tests can assert on
/// partial output without waiting for the process to exit.
struct CliWatcher {
    child: Child,
    buffer: std::sync::Arc<std::sync::Mutex<String>>,
}

impl CliWatcher {
    fn spawn(base_url: &str, token: &str) -> Self {
        let binary = std::env::var_os("CARGO_BIN_EXE_synapse")
            .expect("CARGO_BIN_EXE_synapse must be set; run via `cargo test`");

        let mut child = StdCommand::new(binary)
            .args(["--url", base_url, "events", "watch", "--token", token])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("synapse binary should start");

        let stdout = child.stdout.take().expect("piped stdout");
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let buffer_writer = buffer.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let mut guard = buffer_writer.lock().unwrap();
                guard.push_str(&line);
                guard.push('\n');
            }
        });

        Self { child, buffer }
    }

    /// Polls the accumulated stdout until it contains `needle` or `timeout` elapses.
    fn wait_for_output(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.buffer.lock().unwrap().contains(needle) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Polls until the process exits or `timeout` elapses; returns the exit
    /// status if it exited in time.
    fn wait_for_exit(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for CliWatcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

fn make_update(status: &str) -> TransactionStatusUpdate {
    TransactionStatusUpdate {
        transaction_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        status: status.to_string(),
        timestamp: Utc::now(),
        message: Some(format!("integration test: {status}")),
    }
}

// ── Test 1: connect, receive updates, survive a real ping/pong heartbeat ────

/// Drives the real CLI against the real server across a full heartbeat
/// cycle: connect, receive a `TransactionStatusUpdate`, wait past the
/// server's 30s ping interval (during which tokio-tungstenite must
/// transparently answer the server's Ping with a Pong for the connection to
/// survive), then receive a second update. If ping/pong keepalive silently
/// broke across a tungstenite major, the server would close the connection
/// after the 10s pong timeout and the second update would never arrive.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Postgres + Redis; see module docs"]
async fn real_server_delivers_updates_across_ping_pong_heartbeat() {
    let (base_url, state) = spawn_real_server().await;
    let watcher = CliWatcher::spawn(&base_url, "integration-test-token");

    // Give the WS upgrade a moment to complete before broadcasting.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let first = make_update("pending");
    state
        .tx_broadcast
        .send(first.clone())
        .expect("broadcast first update");

    assert!(
        watcher.wait_for_output(&first.transaction_id.to_string(), Duration::from_secs(10)),
        "CLI did not print the first TransactionStatusUpdate"
    );

    // Wait past one full heartbeat interval + pong timeout margin so at
    // least one server Ping / client Pong round-trip has happened.
    tokio::time::sleep(HEARTBEAT_INTERVAL + Duration::from_secs(5)).await;

    let second = make_update("completed");
    state
        .tx_broadcast
        .send(second.clone())
        .expect("broadcast second update — connection should still be alive");

    assert!(
        watcher.wait_for_output(&second.transaction_id.to_string(), Duration::from_secs(10)),
        "CLI did not print the second update — connection likely dropped across the heartbeat, \
         indicating ping/pong keepalive is broken"
    );
}

// ── Test 2: real server, non-ponging raw client → explicit server close ────

/// Connects to the *real* server with a raw socket that completes the WS
/// handshake but never answers the server's heartbeat Ping. This exercises
/// the real PONG_TIMEOUT code path in `src/handlers/ws.rs` and confirms the
/// server now sends an explicit RFC 6455 Close frame (rather than just
/// dropping the TCP connection) once it gives up on the client.
///
/// A raw client — not the real CLI — is required here because
/// tokio-tungstenite auto-answers Pings transparently (verified by the
/// heartbeat test above), so the real CLI structurally cannot be made to
/// trigger this path.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Postgres + Redis; waits past PONG_TIMEOUT — see module docs"]
async fn real_server_pong_timeout_sends_explicit_close_frame() {
    let (base_url, _state) = spawn_real_server().await;
    let host = base_url.trim_start_matches("http://");

    let stream = TcpStream::connect(host)
        .await
        .expect("connect to real server");
    let request = common::upgrade_request(host, "/ws?token=integration-test-token");

    // Wrap the stream in a single BufReader for the whole connection: mixing
    // a BufReader (for the HTTP response lines) with direct reads on the raw
    // stream (for WS frames) would risk the BufReader silently swallowing
    // frame bytes it over-read past the header.
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
    let mut reader = TokioBufReader::new(stream);
    reader
        .get_mut()
        .write_all(request.as_bytes())
        .await
        .unwrap();

    // Read (and discard) the HTTP upgrade response headers.
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        if n == 0 || line == "\r\n" {
            break;
        }
    }

    // Never send a Pong. Wait for the server to notice and close.
    let deadline = HEARTBEAT_INTERVAL + PONG_TIMEOUT + Duration::from_secs(10);
    let result = tokio::time::timeout(deadline, async {
        loop {
            let (_, opcode, _) = common::read_frame(&mut reader).await.expect("read frame");
            if opcode == common::OPCODE_CLOSE {
                return;
            }
            // Ignore Ping frames (opcode 0x9) and anything else while waiting.
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "server did not send an explicit Close frame within {:?} of going silent",
        deadline
    );
}

// ── Test 3: fragmented (multi-frame) message + close-frame CLI handling ────

/// Neither axum's `ws` feature nor tokio-tungstenite auto-fragments outgoing
/// messages, so the real server can never organically produce a multi-frame
/// message today ("if the server ever sends one" per the coverage
/// requirement). This test hand-crafts a genuine two-frame fragmented
/// `TransactionStatusUpdate` (FIN=0 text frame + FIN=1 continuation frame)
/// from a purpose-built raw responder and confirms the real CLI correctly
/// reassembles it — proving the *client's* tokio-tungstenite layer handles
/// fragmentation regardless of whether the server exercises it today. The
/// same responder also sends an explicit Close frame afterward, verifying
/// the real CLI reacts to a server-initiated close by exiting cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn cli_reassembles_fragmented_message_and_exits_on_close() {
    let port = free_port();
    let addr = format!("127.0.0.1:{}", port);

    let transaction_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();
    let payload = format!(
        r#"{{"transaction_id":"{}","tenant_id":"{}","status":"pending","timestamp":"2026-06-29T10:00:00Z","message":"fragmented"}}"#,
        transaction_id, tenant_id
    );

    tokio::spawn(async move {
        let listener = TcpListener::bind(&addr).await.expect("bind");
        let Ok(accepted) = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await
        else {
            return;
        };
        let Ok((mut stream, _)) = accepted else {
            return;
        };

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let _ = stream
            .write_all(common::upgrade_response(&request).as_bytes())
            .await;

        // Split the payload roughly in half and send it as two frames: a
        // non-final Text frame, then a final Continuation frame.
        let mid = payload.len() / 2;
        let (first_half, second_half) = payload.as_bytes().split_at(mid);
        let frame1 = common::ws_frame(false, common::OPCODE_TEXT, first_half);
        let frame2 = common::ws_frame(true, common::OPCODE_CONTINUATION, second_half);
        let _ = stream.write_all(&frame1).await;
        let _ = stream.write_all(&frame2).await;

        // Server-initiated close.
        let close_frame = common::ws_frame(true, common::OPCODE_CLOSE, &[]);
        let _ = stream.write_all(&close_frame).await;

        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let mut drain = [0u8; 128];
            let _ = stream.read(&mut drain).await;
        })
        .await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let base_url = format!("http://127.0.0.1:{}", port);
    let mut watcher = CliWatcher::spawn(&base_url, "integration-test-token");

    assert!(
        watcher.wait_for_output(&transaction_id.to_string(), Duration::from_secs(5)),
        "CLI did not print the reassembled fragmented TransactionStatusUpdate"
    );

    let status = watcher.wait_for_exit(Duration::from_secs(5));
    assert!(
        status.is_some_and(|s| s.success()),
        "CLI did not exit cleanly after receiving the server's Close frame"
    );
}
