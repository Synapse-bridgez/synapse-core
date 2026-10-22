use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Args)]
pub struct ComplianceArgs {
    /// Machine-readable control file (source of truth).
    #[arg(long, default_value = "docs/compliance/controls.json")]
    controls: PathBuf,
    /// Repository root that evidence refs are resolved against.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Evidence period key, e.g. 2026-10 (artifacts are keyed by control id and period).
    #[arg(long)]
    period: Option<String>,
    /// Directory to write evidence artifacts into.
    #[arg(long, default_value = "compliance-evidence")]
    out: PathBuf,
    /// Write the human-readable matrix to this path instead of collecting evidence.
    #[arg(long)]
    render: Option<PathBuf>,
    /// Fail if the matrix at this path differs from the one generated from the control file.
    #[arg(long)]
    check_matrix: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct ControlFile {
    schema_version: u32,
    retention_days: u32,
    open_findings: Vec<OpenFinding>,
    controls: Vec<Control>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenFinding {
    id: String,
    owner: String,
    summary: String,
}

#[derive(Debug, Deserialize)]
struct Control {
    id: String,
    title: String,
    status: String,
    automation: String,
    implementation: String,
    enforced_by: Vec<String>,
    evidence: Vec<Source>,
    owner: String,
    finding: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Source {
    kind: String,
    #[serde(rename = "ref")]
    reference: String,
    description: String,
    required: bool,
}

#[derive(Debug, Serialize, PartialEq)]
struct SourceResult {
    reference: String,
    kind: String,
    present: bool,
    bytes: Option<u64>,
}

#[derive(Debug, Serialize, PartialEq)]
struct Artifact {
    control_id: String,
    period: String,
    collected_at: String,
    retention_days: u32,
    /// "pass" only when every required source exists and the control is not a gap.
    result: String,
    sources: Vec<SourceResult>,
    findings: Vec<String>,
}

fn load(path: &Path) -> Result<ControlFile> {
    let file: ControlFile = serde_json::from_str(
        &fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
    )
    .context("parsing control file")?;
    validate(&file)?;
    Ok(file)
}

fn validate(file: &ControlFile) -> Result<()> {
    if file.schema_version != 1 {
        bail!("unsupported schema_version {}", file.schema_version);
    }
    let mut seen = std::collections::HashSet::new();
    for c in &file.controls {
        if !seen.insert(&c.id) {
            bail!("duplicate control id {}", c.id);
        }
        if !["implemented", "partial", "gap"].contains(&c.status.as_str()) {
            bail!("{}: invalid status {}", c.id, c.status);
        }
        if !["automated", "manual"].contains(&c.automation.as_str()) {
            bail!("{}: invalid automation {}", c.id, c.automation);
        }
        if c.owner.is_empty() {
            bail!("{}: missing owner", c.id);
        }
        if c.status != "implemented" && c.finding.as_deref().unwrap_or("").is_empty() {
            bail!("{}: {} control must carry a finding", c.id, c.status);
        }
        if c.status != "gap" && c.evidence.is_empty() {
            bail!("{}: non-gap control has no evidence source", c.id);
        }
        if c.automation == "automated"
            && !c
                .evidence
                .iter()
                .any(|s| s.kind == "ci_job" || s.kind == "file")
        {
            bail!(
                "{}: marked automated without a machine-checkable source",
                c.id
            );
        }
    }
    Ok(())
}

fn collect(c: &Control, root: &Path, period: &str, retention_days: u32) -> Artifact {
    let mut findings: Vec<String> = c.finding.iter().cloned().collect();
    let mut sources = Vec::new();
    for s in &c.evidence {
        let meta = fs::metadata(root.join(&s.reference)).ok();
        let present = meta.is_some();
        if !present && s.required {
            findings.push(format!(
                "required evidence source missing: {} ({})",
                s.reference, s.description
            ));
        }
        sources.push(SourceResult {
            reference: s.reference.clone(),
            kind: s.kind.clone(),
            present,
            bytes: meta.map(|m| m.len()),
        });
    }
    for path in &c.enforced_by {
        if !root.join(path).exists() {
            findings.push(format!("enforcing file missing: {path}"));
        }
    }
    let result = if c.status == "gap" || !findings.is_empty() {
        "fail"
    } else {
        "pass"
    };
    Artifact {
        control_id: c.id.clone(),
        period: period.to_string(),
        collected_at: Utc::now().to_rfc3339(),
        retention_days,
        result: result.to_string(),
        sources,
        findings,
    }
}

/// Rendered from the control file so the matrix cannot drift from it.
fn render_matrix(file: &ControlFile) -> String {
    let mut out = String::from(
		"<!-- Generated by `cargo run -p xtask -- compliance --render docs/compliance/control-matrix.md`; edit controls.json, not this file. -->\n# SOC 2 Control Matrix: Swap Engine\n\n| Control | Title | Status | Automation | Implementation / enforced by | Evidence | Owner |\n|---|---|---|---|---|---|---|\n",
	);
    for c in &file.controls {
        let enforced = if c.enforced_by.is_empty() {
            "-".to_string()
        } else {
            c.enforced_by
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let evidence = if c.evidence.is_empty() {
            "none".to_string()
        } else {
            c.evidence
                .iter()
                .map(|s| format!("`{}` ({})", s.reference, s.kind))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {}; {} | {} | {} |\n",
            c.id, c.title, c.status, c.automation, c.implementation, enforced, evidence, c.owner
        ));
    }
    out.push_str("\n## Open findings\n\n");
    for c in file.controls.iter().filter(|c| c.finding.is_some()) {
        out.push_str(&format!(
            "- **{}** (owner: {}): {}\n",
            c.id,
            c.owner,
            c.finding.as_deref().unwrap_or("")
        ));
    }
    for f in &file.open_findings {
        out.push_str(&format!(
            "- **{}** (owner: {}): {}\n",
            f.id, f.owner, f.summary
        ));
    }
    out
}

pub fn run(args: ComplianceArgs) -> Result<()> {
    let file = load(&args.controls)?;
    let matrix = render_matrix(&file);
    if let Some(path) = &args.render {
        fs::write(path, &matrix)?;
        println!("wrote {}", path.display());
        return Ok(());
    }
    if let Some(path) = &args.check_matrix {
        if fs::read_to_string(path).unwrap_or_default() != matrix {
            bail!(
                "{} is out of date; regenerate with --render",
                path.display()
            );
        }
        println!("matrix up to date");
        return Ok(());
    }

    let period = args
        .period
        .unwrap_or_else(|| Utc::now().format("%Y-%m").to_string());
    let dir = args.out.join(&period);
    fs::create_dir_all(&dir)?;
    let mut failed = Vec::new();
    for c in &file.controls {
        let artifact = collect(c, &args.root, &period, file.retention_days);
        // Idempotent: one artifact per (control id, period); rerun overwrites.
        fs::write(
            dir.join(format!("{}.json", c.id)),
            serde_json::to_string_pretty(&artifact)?,
        )?;
        if artifact.result != "pass" {
            failed.push(c.id.clone());
        }
    }
    println!(
        "collected {} controls into {}",
        file.controls.len(),
        dir.display()
    );
    if !failed.is_empty() {
        bail!("controls with open findings: {}", failed.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    fn control(status: &str, evidence_ref: &str) -> Control {
        Control {
            id: "T-1".into(),
            title: "t".into(),
            status: status.into(),
            automation: "manual".into(),
            implementation: "i".into(),
            enforced_by: vec![],
            owner: "o".into(),
            finding: if status == "implemented" {
                None
            } else {
                Some("f".into())
            },
            evidence: vec![Source {
                kind: "file".into(),
                reference: evidence_ref.into(),
                description: "d".into(),
                required: true,
            }],
        }
    }

    #[test]
    fn repo_control_file_is_valid_and_sources_resolve() {
        let file = load(&repo_root().join("docs/compliance/controls.json")).unwrap();
        for c in file.controls.iter().filter(|c| c.status != "gap") {
            let a = collect(c, &repo_root(), "2026-10", file.retention_days);
            assert!(
                a.sources.iter().all(|s| s.present),
                "{} has unresolved evidence: {:?}",
                c.id,
                a.findings
            );
        }
    }

    #[test]
    fn committed_matrix_matches_control_file() {
        let file = load(&repo_root().join("docs/compliance/controls.json")).unwrap();
        let committed =
            fs::read_to_string(repo_root().join("docs/compliance/control-matrix.md")).unwrap();
        assert_eq!(committed, render_matrix(&file));
    }

    #[test]
    fn missing_evidence_source_is_a_finding_not_a_pass() {
        let a = collect(
            &control("implemented", "does/not/exist.md"),
            &repo_root(),
            "2026-10",
            400,
        );
        assert_eq!(a.result, "fail");
        assert!(a.findings.iter().any(|f| f.contains("does/not/exist.md")));
        assert!(!a.sources[0].present);
    }

    #[test]
    fn gap_controls_never_pass_and_need_findings() {
        let a = collect(&control("gap", "Cargo.toml"), &repo_root(), "2026-10", 400);
        assert_eq!(a.result, "fail");
        let mut bad = control("partial", "Cargo.toml");
        bad.finding = None;
        let file = ControlFile {
            schema_version: 1,
            retention_days: 1,
            open_findings: vec![],
            controls: vec![bad],
        };
        assert!(validate(&file).is_err());
    }
}
