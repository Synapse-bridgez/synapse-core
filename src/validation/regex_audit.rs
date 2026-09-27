//! Regression guard for the regex pattern audit (issue #1313).
//!
//! `docs/regex-pattern-audit.md` classifies every regex pattern source in
//! `src/` as static (developer-authored, safe by review) or dynamic
//! (operator/tenant-supplied, needs runtime protection). At the time of the
//! audit there are no dynamic sources. These tests keep it that way:
//!
//! * every `Regex::new` / `RegexBuilder::new` / `RegexSet::new` call must take
//!   a string literal or a const on the reviewed allowlist below, so a pattern
//!   built from runtime input fails CI until the audit is updated;
//! * every JSON-schema `pattern` / `patternProperties` key is run by
//!   `jsonschema`'s backtracking engine (`fancy-regex`), so each one must be
//!   accepted by the linear-time `regex` crate (no lookaround or
//!   backreferences, which makes `fancy-regex` delegate to it) and must not
//!   contain a nested unbounded quantifier.

use super::schemas::{callback_schema_v1, webhook_schema_v1};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Regex constructors whose pattern argument is audited.
const CONSTRUCTORS: &[&str] = &["Regex::new(", "RegexBuilder::new(", "RegexSet::new("];

/// Named consts reviewed in `docs/regex-pattern-audit.md` and allowed as a
/// constructor argument. Each entry must still be used, see
/// `reviewed_pattern_consts_are_still_used`.
const REVIEWED_PATTERN_CONSTS: &[&str] = &["IDENTIFIER_PATTERN"];

// === Source scan

#[derive(Debug)]
struct ConstructorCall {
    location: String,
    argument: String,
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("src/ is readable");
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn constructor_calls() -> Vec<ConstructorCall> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);

    let mut calls = Vec::new();
    for file in files {
        // This file names the constructors in string literals.
        if file.ends_with("validation/regex_audit.rs") {
            continue;
        }
        let text = fs::read_to_string(&file).expect("source file is readable");
        for (idx, line) in text.lines().enumerate() {
            for ctor in CONSTRUCTORS {
                let mut rest = line;
                while let Some(pos) = rest.find(ctor) {
                    let after = &rest[pos + ctor.len()..];
                    calls.push(ConstructorCall {
                        location: format!("{}:{}", file.display(), idx + 1),
                        argument: after.trim_start().to_string(),
                    });
                    rest = after;
                }
            }
        }
    }
    calls
}

fn is_string_literal(argument: &str) -> bool {
    argument.starts_with('"') || argument.starts_with("r\"") || argument.starts_with("r#")
}

fn leading_identifier(argument: &str) -> &str {
    let end = argument
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(argument.len());
    &argument[..end]
}

// === Pattern analysis

/// Returns `true` if `c` (the char after an atom or group) starts a quantifier
/// that is unbounded (`*`, `+`, `{n,}`).
fn unbounded_quantifier_at(chars: &[char], i: usize) -> bool {
    match chars.get(i) {
        Some('*') | Some('+') => true,
        Some('{') => {
            let close = chars[i..].iter().position(|&c| c == '}');
            match close {
                Some(len) => chars[i + 1..i + len].last() == Some(&','),
                None => false,
            }
        }
        _ => false,
    }
}

/// Detects the classic catastrophic-backtracking shape: a group that contains
/// an unbounded quantifier and is itself repeated by an unbounded quantifier,
/// e.g. `(a+)+`, `(\d*)*`, `(x+){2,}`. Escapes and character classes are
/// skipped. Overlapping alternation such as `(a|aa)+` is not detected; see the
/// audit doc.
fn has_nested_unbounded_quantifier(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    // One flag per open group (plus the top level): does it contain an
    // unbounded quantifier?
    let mut stack: Vec<bool> = vec![false];
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 2;
                if unbounded_quantifier_at(&chars, i) {
                    *stack.last_mut().expect("stack never empty") = true;
                }
                continue;
            }
            '[' => {
                i += 1;
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                if unbounded_quantifier_at(&chars, i) {
                    *stack.last_mut().expect("stack never empty") = true;
                }
                continue;
            }
            '(' => stack.push(false),
            ')' => {
                let inner = if stack.len() > 1 {
                    stack.pop().expect("checked length")
                } else {
                    false
                };
                let outer = unbounded_quantifier_at(&chars, i + 1);
                if inner && outer {
                    return true;
                }
                let top = stack.last_mut().expect("stack never empty");
                *top = *top || inner || outer;
            }
            _ => {
                if unbounded_quantifier_at(&chars, i + 1) {
                    *stack.last_mut().expect("stack never empty") = true;
                }
            }
        }
        i += 1;
    }
    false
}

fn collect_schema_patterns(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                match (key.as_str(), child) {
                    ("pattern", Value::String(p)) => out.push(p.clone()),
                    ("patternProperties", Value::Object(props)) => {
                        out.extend(props.keys().cloned());
                        for prop in props.values() {
                            collect_schema_patterns(prop, out);
                        }
                    }
                    _ => collect_schema_patterns(child, out),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_schema_patterns(item, out);
            }
        }
        _ => {}
    }
}

fn schema_patterns() -> Vec<String> {
    let mut patterns = Vec::new();
    collect_schema_patterns(&callback_schema_v1(), &mut patterns);
    collect_schema_patterns(&webhook_schema_v1(), &mut patterns);
    patterns
}

// === Tests

#[test]
fn every_regex_constructor_takes_a_static_pattern() {
    let calls = constructor_calls();
    assert!(
        !calls.is_empty(),
        "source scan found no regex constructors; the scan itself is broken"
    );

    let dynamic: Vec<&ConstructorCall> = calls
        .iter()
        .filter(|call| {
            !is_string_literal(&call.argument)
                && !REVIEWED_PATTERN_CONSTS.contains(&leading_identifier(&call.argument))
        })
        .collect();

    assert!(
        dynamic.is_empty(),
        "regex constructor with a non-literal pattern (possible dynamic/ReDoS \
         source). Classify it in docs/regex-pattern-audit.md and either use a \
         literal, add a reviewed const to REVIEWED_PATTERN_CONSTS, or add \
         runtime complexity bounds: {dynamic:#?}"
    );
}

#[test]
fn reviewed_pattern_consts_are_still_used() {
    let calls = constructor_calls();
    for name in REVIEWED_PATTERN_CONSTS {
        assert!(
            calls
                .iter()
                .any(|call| leading_identifier(&call.argument) == *name),
            "{name} is on REVIEWED_PATTERN_CONSTS but no longer used; remove it \
             from the allowlist and docs/regex-pattern-audit.md"
        );
    }
}

#[test]
fn schema_patterns_are_found() {
    // Guards the collector: the callback schema has three `pattern` keywords.
    assert_eq!(schema_patterns().len(), 3);
}

#[test]
fn schema_patterns_run_on_the_linear_time_engine() {
    for pattern in schema_patterns() {
        assert!(
            regex::Regex::new(&pattern).is_ok(),
            "schema pattern {pattern:?} needs fancy-regex features (lookaround or \
             backreferences), so jsonschema would run it on the backtracking engine"
        );
    }
}

#[test]
fn schema_patterns_have_no_nested_unbounded_quantifiers() {
    for pattern in schema_patterns() {
        assert!(
            !has_nested_unbounded_quantifier(&pattern),
            "schema pattern {pattern:?} has a nested unbounded quantifier (ReDoS shape)"
        );
    }
}

#[test]
fn nested_quantifier_detector_flags_catastrophic_shapes() {
    for pattern in [
        "(a+)+",
        "(a*)*",
        r"^(\d+)*$",
        "(x+){2,}",
        "((ab)*c)+",
        "([a-z]+)*",
    ] {
        assert!(
            has_nested_unbounded_quantifier(pattern),
            "{pattern:?} should be flagged"
        );
    }
}

#[test]
fn nested_quantifier_detector_accepts_safe_shapes() {
    for pattern in [
        r"^[0-9]+(\.[0-9]+)?$",
        "^G[A-Z2-7]{55}$",
        "^[A-Z]{3,12}$",
        "(ab){2,}",
        "(a+){2,5}",
        r"\(a+\)+",
        "[(a+)+]",
        "[]a+]+",
    ] {
        assert!(
            !has_nested_unbounded_quantifier(pattern),
            "{pattern:?} should not be flagged"
        );
    }
}
