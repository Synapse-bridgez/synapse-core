//! Regression guard for the CSRF exposure audit (issue #1310).
//!
//! `docs/csrf-exposure-audit.md` enumerates every state-changing route and
//! how it authenticates. At the time of the audit no route reads or sets a
//! cookie, so no route carries an ambient browser credential and none is
//! CSRF-exposed. `main.rs` enables credentialed CORS
//! (`allow_credentials(true)`) when origins are configured, so the moment
//! cookie-based auth appears, cross-site requests from those origins would
//! carry it. This test fails as soon as cookie handling is introduced
//! anywhere in `src/`, so the CSRF defence (SameSite, token or origin check)
//! is designed in the same change instead of discovered later.

use std::fs;
use std::path::{Path, PathBuf};

/// Case-insensitive markers of reading or setting cookies.
const COOKIE_MARKERS: &[&str] = &[
    "header::cookie",
    "header::set_cookie",
    "\"cookie\"",
    "\"set-cookie\"",
    "cookiejar",
    "samesite",
    "tower_cookies",
    "axum_extra::extract::cookie",
];

// === Source scan

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src/ is readable") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn cookie_marker_hits(text: &str) -> Vec<(usize, &'static str)> {
    let mut hits = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        for marker in COOKIE_MARKERS {
            if lower.contains(marker) {
                hits.push((idx + 1, *marker));
            }
        }
    }
    hits
}

// === Tests

#[test]
fn no_route_uses_cookie_based_authentication() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(
        files.len() > 10,
        "source scan found almost no files; the scan itself is broken"
    );

    let mut hits = Vec::new();
    for file in files {
        // This file names the markers in string literals.
        if file.ends_with("security/csrf_audit.rs") {
            continue;
        }
        let text = fs::read_to_string(&file).expect("source file is readable");
        for (line, marker) in cookie_marker_hits(&text) {
            hits.push(format!("{}:{line} ({marker})", file.display()));
        }
    }

    assert!(
        hits.is_empty(),
        "cookie handling found. Cookie-authenticated state-changing routes are \
         CSRF-exposed (main.rs enables credentialed CORS). Add SameSite=Strict/Lax \
         plus a token or origin check, and update docs/csrf-exposure-audit.md: {hits:#?}"
    );
}

#[test]
fn marker_scan_detects_cookie_handling() {
    for line in [
        "let c = headers.get(header::COOKIE);",
        "res.headers_mut().insert(header::SET_COOKIE, v);",
        "headers.get(\"Cookie\")",
        "headers.get(\"set-cookie\")",
        "async fn h(jar: CookieJar) {}",
        "Cookie::build((\"s\", v)).same_site(SameSite::Lax)",
        "use axum_extra::extract::cookie::PrivateCookieJar;",
    ] {
        assert!(
            !cookie_marker_hits(line).is_empty(),
            "{line:?} should be detected"
        );
    }
}

#[test]
fn marker_scan_ignores_header_based_auth() {
    for line in [
        "req.headers().get(\"Authorization\")",
        "headers.get(\"X-API-Key\")",
        "headers.get(\"X-Stellar-Signature\")",
        "// session validation, no cookies involved",
    ] {
        assert!(
            cookie_marker_hits(line).is_empty(),
            "{line:?} should not be detected"
        );
    }
}
