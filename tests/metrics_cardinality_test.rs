//! Static enforcement of the label-cardinality convention documented in
//! `docs/metrics-cardinality-convention.md`. Fails if any `src/` file
//! records a metric label using a known-unbounded label name (raw ID,
//! raw count, URL, etc.) that was not reviewed and added to
//! `ALLOWED_EXCEPTIONS`.

use std::fs;
use std::path::Path;

/// Label names that are always unbounded and must never be used as a
/// `KeyValue` label. Keep in sync with docs/metrics-cardinality-convention.md.
const DENYLISTED_LABELS: &[&str] = &[
    "user_id",
    "tenant_id",
    "transaction_id",
    "account_id",
    "request_id",
    "session_id",
    "url",
    "path",
    "count",
    "transaction_count",
];

/// `(file, label)` pairs that were reviewed and found to be bounded despite
/// matching a denylisted substring, or are intentionally exempt. Empty:
/// no exceptions have been reviewed yet — see
/// docs/metrics-cardinality-convention.md.
const ALLOWED_EXCEPTIONS: &[(&str, &str)] = &[];

fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_unbounded_cardinality_metric_labels() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files);
    assert!(!files.is_empty(), "expected to find .rs files under src/");

    let mut violations = Vec::new();

    for file in &files {
        let contents = fs::read_to_string(file).unwrap_or_default();
        let rel = file
            .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");

        for (line_no, line) in contents.lines().enumerate() {
            let Some(idx) = line.find("KeyValue::new(\"") else {
                continue;
            };
            let after = &line[idx + "KeyValue::new(\"".len()..];
            let Some(end) = after.find('"') else {
                continue;
            };
            let label = &after[..end];

            if DENYLISTED_LABELS.contains(&label)
                && !ALLOWED_EXCEPTIONS.contains(&(rel.as_str(), label))
            {
                violations.push(format!("{rel}:{} — label \"{label}\"", line_no + 1));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "found metric labels with known-unbounded cardinality (see \
         docs/metrics-cardinality-convention.md — either bucket/rename the \
         label or add a reviewed exception):\n{}",
        violations.join("\n")
    );
}
