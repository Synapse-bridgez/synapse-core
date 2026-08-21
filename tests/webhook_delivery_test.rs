use hmac::{Hmac, Mac};
use mockito::Server;
use reqwest::StatusCode;
use sha2::Sha256;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug)]
struct DeliveryResult {
    status: StatusCode,
    attempts: usize,
}

struct TestDispatcher {
    client: reqwest::Client,
    secret: String,
    max_retries: usize,
}

impl TestDispatcher {
    fn new(secret: &str, timeout: Duration, max_retries: usize) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("client build");

        Self {
            client,
            secret: secret.to_string(),
            max_retries,
        }
    }

    fn signature_for(&self, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    async fn send(&self, url: &str, body: &str) -> Result<DeliveryResult, reqwest::Error> {
        let mut attempts = 0usize;
        let body_bytes = body.as_bytes();
        let sig = self.signature_for(body_bytes);

        loop {
            attempts += 1;
            let req = self
                .client
                .post(url)
                .header("X-Stellar-Signature", sig.clone())
                .body(body.to_string());

            let resp = req.send().await;

            match resp {
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        return Ok(DeliveryResult { status, attempts });
                    }
                    // retry on 5xx
                    if status.is_server_error() && attempts <= self.max_retries {
                        // small backoff
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                    return Ok(DeliveryResult { status, attempts });
                }
                Err(e) => {
                    // treat timeout / connect errors as retryable until max_retries
                    if attempts <= self.max_retries {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }
}

#[tokio::test]
async fn test_webhook_delivery_success() {
    let mut server = Server::new_async().await;
    let _m = server.mock("POST", "/success").with_status(200).create();

    let url = format!("{}/success", server.url());
    let d = TestDispatcher::new("secret", Duration::from_secs(2), 3);
    let res = d.send(&url, r#"{"ok":true}"#).await.expect("send");
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.attempts, 1);
}

#[tokio::test]
async fn test_webhook_retry_on_failure() {
    let mut server = Server::new_async().await;
    // first request: 500, second: 200
    let _m1 = server
        .mock("POST", "/retry")
        .with_status(500)
        .expect(1)
        .create();

    let _m2 = server
        .mock("POST", "/retry")
        .with_status(200)
        .expect(1)
        .create();

    let url = format!("{}/retry", server.url());
    let d = TestDispatcher::new("secret", Duration::from_secs(2), 3);
    let res = d.send(&url, "{}").await.expect("send");
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.attempts >= 2);
}

#[tokio::test]
async fn test_webhook_signature_generation() {
    let d = TestDispatcher::new("my-secret", Duration::from_secs(2), 0);
    let body = b"payload";
    let sig = d.signature_for(body);

    // Compute expected via HMAC crate directly
    let mut mac = HmacSha256::new_from_slice(b"my-secret").unwrap();
    mac.update(body);
    let expected = hex::encode(mac.finalize().into_bytes());

    assert_eq!(sig, expected);
}

#[tokio::test]
async fn test_webhook_delivery_timeout() {
    // create a simple TCP listener that accepts and delays response
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let addr = listener.local_addr().unwrap();

    // spawn thread to accept one connection and sleep
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        // read request (ignore errors)
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        // sleep longer than client timeout
        thread::sleep(Duration::from_secs(2));
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
    });

    let url = format!("http://{}:{}/slow", addr.ip(), addr.port());
    // set client timeout shorter than server sleep
    let d = TestDispatcher::new("s", Duration::from_millis(200), 0);

    let start = Instant::now();
    let res = d.send(&url, "{}").await;
    let elapsed = start.elapsed();

    // should be a timeout error
    assert!(res.is_err());
    // elapsed should be less than the server sleep (i.e., the client timed out)
    assert!(elapsed < Duration::from_secs(2));
}

#[tokio::test]
async fn test_webhook_delivery_tracking() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("POST", "/track")
        .with_status(500)
        .expect(3)
        .create();

    let url = format!("{}/track", server.url());
    let d = TestDispatcher::new("s", Duration::from_secs(2), 2);

    let res = d.send(&url, "{}").await.expect("send");
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.attempts, 3);
}

#[tokio::test]
async fn test_concurrent_webhook_deliveries() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("POST", "/concurrent")
        .with_status(200)
        .expect(10)
        .create();

    let url = format!("{}/concurrent", server.url());
    let d = Arc::new(TestDispatcher::new("secret", Duration::from_secs(2), 1));
    let mut handles = Vec::new();
    for _ in 0..10 {
        let dd = d.clone();
        let urlc = url.clone();
        handles.push(tokio::spawn(async move { dd.send(&urlc, "{}").await }));
    }

    for h in handles {
        let r = h.await.expect("task").expect("send");
        assert_eq!(r.status, StatusCode::OK);
    }
}
