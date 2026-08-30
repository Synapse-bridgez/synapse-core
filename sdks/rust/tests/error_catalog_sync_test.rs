//! Cross-checks every error code documented in `docs/error-catalog.md`
//! against `synapse_sdk::ErrorCode`, so the SDK's error taxonomy can't
//! silently drift out of sync with the server's catalog as it grows.
//!
//! A code that maps to `ErrorCode::Unknown` here means the catalog gained a
//! code with no corresponding typed SDK variant — add one in
//! `sdks/rust/src/error.rs` (`ErrorCode` enum and `ErrorCode::from_code`).

use synapse_sdk::ErrorCode;

/// Extracts every `ERR_<CATEGORY>_<NNN>`-shaped token from the catalog doc.
/// Deliberately unstructured (no markdown-table parsing) so it also picks up
/// codes referenced outside the tables (e.g. the "Using Error Codes"
/// examples) — a superset is fine since every hit must map to a known code.
fn catalog_codes() -> Vec<String> {
    let catalog = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/error-catalog.md"
    ));

    let mut codes: Vec<String> = catalog
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| token.starts_with("ERR_"))
        .filter(|token| token.chars().last().map(|c| c.is_ascii_digit()).unwrap_or(false))
        .map(str::to_string)
        .collect();

    codes.sort();
    codes.dedup();
    codes
}

#[test]
fn every_catalog_code_maps_to_a_known_error_code_variant() {
    let codes = catalog_codes();
    assert!(
        codes.len() >= 20,
        "sanity check: expected at least 20 distinct codes parsed from \
         docs/error-catalog.md, found {} — the parser may be broken",
        codes.len()
    );

    let unmapped: Vec<String> = codes
        .into_iter()
        .filter(|code| matches!(ErrorCode::from_code(code), ErrorCode::Unknown(_)))
        .collect();

    assert!(
        unmapped.is_empty(),
        "docs/error-catalog.md documents code(s) with no matching ErrorCode \
         variant: {unmapped:?} — add a variant (and a from_code arm) in \
         sdks/rust/src/error.rs"
    );
}

#[test]
fn unknown_code_degrades_gracefully_instead_of_panicking() {
    let code = ErrorCode::from_code("ERR_SOME_FUTURE_CODE_999");
    assert_eq!(code, ErrorCode::Unknown("ERR_SOME_FUTURE_CODE_999".to_string()));
}
