use anyhow::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Once;
use tempfile::TempDir;
use tokio::time::{Duration, Instant};

static INIT_MOCK_COMMANDS: Once = Once::new();

fn install_mock_commands() {
    INIT_MOCK_COMMANDS.call_once(|| {
        let mock_bin_dir = std::env::temp_dir().join("synapse_core_encryption_test_bin");
        fs::create_dir_all(&mock_bin_dir).expect("failed to create mock bin dir");

        write_script(
            &mock_bin_dir,
            "pg_dump",
            r##"#!/usr/bin/env bash
set -euo pipefail
out=""
for arg in "$@"; do
  case "$arg" in
    --file=*) out="${arg#--file=}" ;;
  esac
done
if [ -z "$out" ]; then
  echo "missing --file" >&2
  exit 1
fi
cat > "$out" <<'SQL'
CREATE TABLE transactions (id BIGINT PRIMARY KEY, amount NUMERIC);
INSERT INTO transactions VALUES (1, 42.00);
SQL
"##,
        );

        write_script(
            &mock_bin_dir,
            "gzip",
            r##"#!/usr/bin/env bash
set -euo pipefail
if [ "$1" != "-c" ]; then
  echo "expected -c" >&2
  exit 1
fi
cat "$2"
"##,
        );

        write_script(
            &mock_bin_dir,
            "gunzip",
            r##"#!/usr/bin/env bash
set -euo pipefail
if [ "$1" != "-c" ]; then
  echo "expected -c" >&2
  exit 1
fi
cat "$2"
"##,
        );

        write_script(
            &mock_bin_dir,
            "openssl",
            r##"#!/usr/bin/env bash
set -euo pipefail
if [ "$1" != "enc" ]; then
  echo "unsupported command" >&2
  exit 1
fi

mode="encrypt"
in=""
out=""
pass=""
shift
while [ "$#" -gt 0 ]; do
  case "$1" in
    -d)
      mode="decrypt"
      ;;
    -in)
      in="$2"
      shift
      ;;
    -out)
      out="$2"
      shift
      ;;
    -pass)
      pass="$2"
      shift
      ;;
  esac
  shift
done

if [ -z "$in" ] || [ -z "$out" ] || [ -z "$pass" ]; then
  echo "missing input/output/pass" >&2
  exit 1
fi

key="${pass#pass:}"
key_crc="$(printf "%s" "$key" | cksum | awk '{print $1}')"

if [ "$mode" = "encrypt" ]; then
  {
    printf "KEYCRC:%s\n" "$key_crc"
    cat "$in"
  } > "$out"
  exit 0
fi

if [ "$key" = "expired-key" ]; then
  echo "key expired" >&2
  exit 1
fi

header="$(head -n 1 "$in")"
stored_crc="${header#KEYCRC:}"
if [ "$stored_crc" != "$key_crc" ]; then
  echo "invalid key" >&2
  exit 1
fi

tail -n +2 "$in" > "$out"
"##,
        );

        write_script(
            &mock_bin_dir,
            "sha256sum",
            r##"#!/usr/bin/env bash
set -euo pipefail
printf "0000000000000000000000000000000000000000000000000000000000000000  %s\n" "$1"
"##,
        );

        write_script(
            &mock_bin_dir,
            "psql",
            r##"#!/usr/bin/env bash
set -euo pipefail
file=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --file)
      file="$2"
      shift
      ;;
  esac
  shift
done
if [ -z "$file" ] || [ ! -f "$file" ]; then
  echo "missing restore file" >&2
  exit 1
fi
exit 0
"##,
        );

        let current_path = std::env::var("PATH").unwrap_or_default();
        let mock_dir = mock_bin_dir.to_string_lossy();
        if !current_path.split(':').any(|p| p == mock_dir) {
            std::env::set_var("PATH", format!("{}:{}", mock_dir, current_path));
        }
    });
}

fn write_script(dir: &Path, name: &str, contents: &str) {
    let path = dir.join(name);
    fs::write(&path, contents).expect("failed to write mock script");

    let mut perms = fs::metadata(&path)
        .expect("failed to read mock script metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("failed to mark mock script executable");
}

fn backup_service(
    backup_dir: PathBuf,
    key: Option<&str>,
) -> synapse_core::services::backup::BackupService {
    synapse_core::services::backup::BackupService::new(
        "postgres://test:test@localhost:5432/test".to_string(),
        backup_dir,
        key.map(ToOwned::to_owned),
    )
}

#[tokio::test]
async fn test_payload_encryption_decryption() -> Result<()> {
    install_mock_commands();
    let temp_dir = TempDir::new()?;
    let service = backup_service(temp_dir.path().to_path_buf(), Some("active-key-v1"));

    let metadata = service
        .create_backup(synapse_core::services::backup::BackupType::Hourly)
        .await?;

    assert!(metadata.encrypted);
    assert!(metadata.filename.ends_with(".sql.gz.enc"));

    service.restore_backup(&metadata.filename).await?;

    Ok(())
}

#[tokio::test]
async fn test_key_rotation() -> Result<()> {
    install_mock_commands();
    let temp_dir = TempDir::new()?;
    let backup_dir = temp_dir.path().to_path_buf();

    let old_service = backup_service(backup_dir.clone(), Some("active-key-v1"));
    let old_backup = old_service
        .create_backup(synapse_core::services::backup::BackupType::Hourly)
        .await?;

    old_service.restore_backup(&old_backup.filename).await?;

    let new_service = backup_service(backup_dir.clone(), Some("active-key-v2"));
    let new_backup = new_service
        .create_backup(synapse_core::services::backup::BackupType::Daily)
        .await?;

    new_service.restore_backup(&new_backup.filename).await?;

    let old_with_new_key = new_service.restore_backup(&old_backup.filename).await;
    assert!(old_with_new_key.is_err());

    Ok(())
}

#[tokio::test]
async fn test_decryption_expired_key() -> Result<()> {
    install_mock_commands();
    let temp_dir = TempDir::new()?;
    let backup_dir = temp_dir.path().to_path_buf();

    let valid_service = backup_service(backup_dir.clone(), Some("active-key-v1"));
    let metadata = valid_service
        .create_backup(synapse_core::services::backup::BackupType::Hourly)
        .await?;

    let expired_service = backup_service(backup_dir, Some("expired-key"));
    let result = expired_service.restore_backup(&metadata.filename).await;

    assert!(result.is_err());
    let error_text = result.unwrap_err().to_string();
    assert!(error_text.contains("openssl decryption failed"));
    assert!(error_text.contains("key expired"));

    Ok(())
}

#[tokio::test]
async fn test_decryption_invalid_key() -> Result<()> {
    install_mock_commands();
    let temp_dir = TempDir::new()?;
    let backup_dir = temp_dir.path().to_path_buf();

    let valid_service = backup_service(backup_dir.clone(), Some("active-key-v1"));
    let metadata = valid_service
        .create_backup(synapse_core::services::backup::BackupType::Monthly)
        .await?;

    let invalid_service = backup_service(backup_dir, Some("wrong-key"));
    let result = invalid_service.restore_backup(&metadata.filename).await;

    assert!(result.is_err());
    let error_text = result.unwrap_err().to_string();
    assert!(error_text.contains("openssl decryption failed"));
    assert!(error_text.contains("invalid key"));

    Ok(())
}

#[tokio::test]
async fn test_encryption_performance() -> Result<()> {
    install_mock_commands();
    let temp_dir = TempDir::new()?;
    let service = backup_service(temp_dir.path().to_path_buf(), Some("active-key-v1"));

    let start = Instant::now();
    let _metadata = service
        .create_backup(synapse_core::services::backup::BackupType::Hourly)
        .await?;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "encryption path exceeded performance budget: {:?}",
        elapsed
    );

    Ok(())
}

#[tokio::test]
async fn test_key_storage_security() -> Result<()> {
    install_mock_commands();
    let temp_dir = TempDir::new()?;
    let backup_dir = temp_dir.path().to_path_buf();
    let encryption_key = "super-sensitive-key-material";

    let service = backup_service(backup_dir.clone(), Some(encryption_key));
    let metadata = service
        .create_backup(synapse_core::services::backup::BackupType::Hourly)
        .await?;

    let encrypted_backup_bytes = tokio::fs::read(backup_dir.join(&metadata.filename)).await?;
    let metadata_json =
        tokio::fs::read_to_string(backup_dir.join(&metadata.filename).with_extension("meta"))
            .await?;

    assert!(!metadata_json.contains(encryption_key));
    assert!(!encrypted_backup_bytes
        .windows(encryption_key.len())
        .any(|w| w == encryption_key.as_bytes()));

    Ok(())
}
