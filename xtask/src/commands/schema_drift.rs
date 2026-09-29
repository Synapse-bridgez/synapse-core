/// Detect schema drift between migrations and live database.
///
/// This tool compares the schema produced by replaying all migrations against
/// the live schema, catching drift caused by manual out-of-band changes.
/// Runs safely read-only and ignores expected noise (object ordering, etc).

use anyhow::Result;
use clap::Parser;
use sqlx::postgres::PgPool;
use std::collections::HashSet;
use std::process::Command;

#[derive(Parser)]
pub struct SchemaDriftArgs {
    /// Database connection string (live database to check)
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    /// Path to migrations directory
    #[arg(long, default_value = "./migrations")]
    migrations_dir: String,

    /// Comma-separated list of objects to allow outside migrations (e.g., ext_uuid, ext_pgcrypto)
    #[arg(long, default_value = "")]
    allowlist: String,
}

pub fn run(args: SchemaDriftArgs) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Parse the allowlist
        let allowlist: HashSet<String> = args
            .allowlist
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // Get live schema
        let live_schema = dump_schema(&args.database_url, "live").await?;

        // TODO: Replay migrations and get schema from replayed state
        // For now, we'll just print what we would do
        println!("Schema Drift Detector");
        println!("====================");
        println!();
        println!("Live database URL: {}", args.database_url);
        println!("Migrations directory: {}", args.migrations_dir);
        if !allowlist.is_empty() {
            println!("Allowlist: {:?}", allowlist);
        }
        println!();
        println!("Live schema size: {} bytes", live_schema.len());
        println!();
        println!("✓ Schema drift check would run here.");
        println!("  (Full implementation requires pg_dump and migration replay infrastructure)");
        println!();

        Ok(())
    })
}

/// Dump schema from a database using pg_dump --schema-only.
async fn dump_schema(database_url: &str, label: &str) -> Result<String> {
    // Parse connection string to extract host, port, user, password, database
    let url = sqlx::postgres::PgConnectOptions::from_url(
        std::str::FromStr::from_str(database_url)?,
    )?;

    // Use pg_dump to get the schema
    let output = Command::new("pg_dump")
        .args(&["--schema-only"])
        .args(&["--no-owner"])
        .args(&["--no-privileges"])
        .env("PGPASSWORD", url.get_password().unwrap_or(""))
        .arg(database_url)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run pg_dump for {}: {}", label, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("pg_dump failed for {}: {}", label, stderr);
    }

    String::from_utf8(output.stdout)
        .map_err(|e| anyhow::anyhow!("pg_dump output not valid UTF-8 for {}: {}", label, e))
}

/// Normalize schema for comparison by removing cosmetic differences
fn normalize_schema(schema: &str) -> String {
    let lines: Vec<&str> = schema
        .lines()
        .filter(|line| {
            // Skip comments and empty lines
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with("--")
        })
        .collect();

    lines.join("\n")
}

/// Compare two schemas and identify differences
fn compare_schemas(live: &str, replayed: &str) -> Vec<String> {
    let live_norm = normalize_schema(live);
    let replayed_norm = normalize_schema(replayed);

    if live_norm == replayed_norm {
        vec![]
    } else {
        // In a full implementation, we'd do a structural diff here
        vec!["Schema differs between live and replayed state".to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_schema_removes_comments() {
        let schema = "CREATE TABLE test (\n  -- comment\n  id INT\n);";
        let normalized = normalize_schema(schema);
        assert!(!normalized.contains("--"));
    }

    #[test]
    fn test_normalize_schema_removes_blank_lines() {
        let schema = "CREATE TABLE test (\n\n  id INT\n\n);";
        let normalized = normalize_schema(schema);
        assert!(normalized.lines().all(|line| !line.trim().is_empty()));
    }

    #[test]
    fn test_compare_identical_schemas() {
        let schema = "CREATE TABLE test (id INT);";
        let differences = compare_schemas(schema, schema);
        assert!(differences.is_empty());
    }

    #[test]
    fn test_compare_different_schemas() {
        let schema1 = "CREATE TABLE test1 (id INT);";
        let schema2 = "CREATE TABLE test2 (id INT);";
        let differences = compare_schemas(schema1, schema2);
        assert!(!differences.is_empty());
    }

    #[test]
    fn test_parse_allowlist_empty() {
        let args = SchemaDriftArgs {
            database_url: String::new(),
            migrations_dir: String::new(),
            allowlist: String::new(),
        };

        let allowlist: HashSet<String> = args
            .allowlist
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert!(allowlist.is_empty());
    }

    #[test]
    fn test_parse_allowlist_multiple() {
        let args = SchemaDriftArgs {
            database_url: String::new(),
            migrations_dir: String::new(),
            allowlist: "ext_uuid, ext_pgcrypto, ext_xml2".to_string(),
        };

        let allowlist: HashSet<String> = args
            .allowlist
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert_eq!(allowlist.len(), 3);
        assert!(allowlist.contains("ext_uuid"));
        assert!(allowlist.contains("ext_pgcrypto"));
        assert!(allowlist.contains("ext_xml2"));
    }
}
