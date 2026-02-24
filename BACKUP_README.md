# Database Backup & Restore System

## Overview

Automated database backup system with encryption, compression, and tested restore procedures for disaster recovery.

## Features

- **Scheduled Backups**: Hourly, daily, and monthly backup types
- **Compression**: Automatic gzip compression to reduce storage
- **Encryption**: AES-256-CBC encryption with PBKDF2 key derivation
- **Integrity Verification**: SHA256 checksums for backup validation
- **Retention Policy**: Automatic cleanup (24 hourly, 30 daily, 12 monthly)
- **Restore Testing**: Validated restore procedure with integrity checks

## Configuration

Add to your `.env` file:

```bash
# Backup directory (local filesystem or mount point)
BACKUP_DIR=./backups

# Encryption key (32+ characters recommended)
# IMPORTANT: Store this separately from database credentials
BACKUP_ENCRYPTION_KEY=your-secure-32-character-key-here
```

## CLI Commands

### Create a Backup

```bash
# Hourly backup (default)
cargo run -- backup run

# Daily backup
cargo run -- backup run --backup-type daily

# Monthly backup
cargo run -- backup run --backup-type monthly
```

### List Backups

```bash
cargo run -- backup list
```

Output:
```
Available backups:
Filename                                 Type       Timestamp            Size
-------------------------------------------------------------------------------------
backup_hourly_20260222_143000.sql.gz.enc Hourly     2026-02-22 14:30:00  2.45 MB
backup_daily_20260222_000000.sql.gz.enc  Daily      2026-02-22 00:00:00  2.43 MB
```

### Restore from Backup

```bash
cargo run -- backup restore backup_hourly_20260222_143000.sql.gz.enc
```

**Warning**: This will replace the current database. A 5-second countdown is provided to cancel.

### Apply Retention Policy

```bash
cargo run -- backup cleanup
```

Removes old backups according to retention policy:
- Keep last 24 hourly backups
- Keep last 30 daily backups
- Keep last 12 monthly backups

## Backup Process

1. **pg_dump**: Creates SQL dump of database
2. **Compression**: Compresses with gzip (~70% size reduction)
3. **Encryption**: Encrypts with AES-256-CBC (if key provided)
4. **Checksum**: Calculates SHA256 for integrity verification
5. **Metadata**: Saves backup metadata (timestamp, size, type, checksum)

## Restore Process

1. **Verification**: Validates backup integrity using checksum
2. **Decryption**: Decrypts backup (if encrypted)
3. **Decompression**: Decompresses gzip file
4. **Restore**: Executes SQL using psql
5. **Cleanup**: Removes temporary files

## Backup Types

### Hourly Backups
- Created every hour
- Retention: Last 24 backups
- Use case: Recent point-in-time recovery

### Daily Backups
- Created once per day
- Retention: Last 30 backups
- Use case: Daily snapshots for the past month

### Monthly Backups
- Created once per month
- Retention: Last 12 backups
- Use case: Long-term archival

## Storage Backends

### Local Filesystem
Default configuration stores backups in `./backups` directory.

```bash
BACKUP_DIR=./backups
```

### Network Mount (NFS/SMB)
Mount network storage and point backup directory to it:

```bash
BACKUP_DIR=/mnt/backup-storage
```

### S3-Compatible Storage
Mount S3 bucket using s3fs or similar:

```bash
# Mount S3 bucket
s3fs my-backup-bucket /mnt/s3-backups

# Configure backup directory
BACKUP_DIR=/mnt/s3-backups
```

## Security Considerations

### Encryption Key Management

**CRITICAL**: The encryption key must be stored separately from database credentials.

Recommended approaches:
1. **Environment Variable**: Set in secure environment (not in .env file)
2. **Secrets Manager**: AWS Secrets Manager, HashiCorp Vault, etc.
3. **Key Management Service**: AWS KMS, Google Cloud KMS, etc.

```bash
# Good: Set in secure environment
export BACKUP_ENCRYPTION_KEY=$(aws secretsmanager get-secret-value --secret-id backup-key --query SecretString --output text)

# Bad: Hardcoded in .env file
BACKUP_ENCRYPTION_KEY=my-key-123  # DON'T DO THIS IN PRODUCTION
```

### Backup File Permissions

Ensure backup directory has restricted permissions:

```bash
chmod 700 ./backups
chown postgres:postgres ./backups
```

### Network Transfer

When transferring backups:
- Use encrypted channels (SFTP, SCP, HTTPS)
- Verify checksums after transfer
- Use separate credentials for backup storage

## Disaster Recovery Procedure

### 1. Identify Backup to Restore

```bash
cargo run -- backup list
```

### 2. Verify Backup Integrity

The restore command automatically verifies checksums before restoring.

### 3. Stop Application

```bash
# Stop the application to prevent new writes
systemctl stop synapse-core
```

### 4. Restore Database

```bash
cargo run -- backup restore backup_daily_20260222_000000.sql.gz.enc
```

### 5. Verify Restoration

```bash
# Connect to database and verify data
psql $DATABASE_URL -c "SELECT COUNT(*) FROM transactions;"
```

### 6. Restart Application

```bash
systemctl start synapse-core
```

## Automated Scheduling

### Using Cron

```cron
# Hourly backups
0 * * * * cd /path/to/synapse-core && cargo run -- backup run --backup-type hourly

# Daily backups at midnight
0 0 * * * cd /path/to/synapse-core && cargo run -- backup run --backup-type daily

# Monthly backups on 1st of month
0 0 1 * * cd /path/to/synapse-core && cargo run -- backup run --backup-type monthly

# Cleanup old backups daily
0 1 * * * cd /path/to/synapse-core && cargo run -- backup cleanup
```

### Using systemd Timers

Create `/etc/systemd/system/synapse-backup-hourly.service`:

```ini
[Unit]
Description=Synapse Hourly Backup

[Service]
Type=oneshot
User=postgres
WorkingDirectory=/opt/synapse-core
ExecStart=/usr/local/bin/synapse-core backup run --backup-type hourly
```

Create `/etc/systemd/system/synapse-backup-hourly.timer`:

```ini
[Unit]
Description=Synapse Hourly Backup Timer

[Timer]
OnCalendar=hourly
Persistent=true

[Install]
WantedBy=timers.target
```

Enable and start:

```bash
systemctl enable synapse-backup-hourly.timer
systemctl start synapse-backup-hourly.timer
```

## Monitoring

### Backup Success/Failure

Monitor backup logs:

```bash
journalctl -u synapse-backup-hourly -f
```

### Backup Size Trends

```bash
du -sh ./backups/*
```

### Disk Space

```bash
df -h ./backups
```

### Alerting

Set up alerts for:
- Backup failures
- Disk space < 20%
- Missing backups (no backup in last 2 hours)
- Checksum verification failures

## Testing

Run integration tests:

```bash
cargo test backup_test
```

Tests cover:
- Backup creation
- Backup listing
- Restore procedure
- Retention policy
- Encryption/decryption
- Checksum verification

## Troubleshooting

### pg_dump not found

```bash
# Install PostgreSQL client tools
apt-get install postgresql-client  # Debian/Ubuntu
yum install postgresql             # RHEL/CentOS
```

### Encryption fails

Ensure OpenSSL is installed:

```bash
openssl version
```

### Insufficient disk space

```bash
# Check available space
df -h ./backups

# Run cleanup
cargo run -- backup cleanup
```

### Restore fails

Check database permissions:

```sql
-- User needs CREATEDB privilege
ALTER USER synapse CREATEDB;
```

## Performance Considerations

### Backup Duration

- Small DB (< 1GB): ~30 seconds
- Medium DB (1-10GB): 1-5 minutes
- Large DB (> 10GB): 5+ minutes

### Compression Ratio

Typical compression: 60-80% size reduction

### Encryption Overhead

Minimal impact: < 5% additional time

## Compliance

This backup system supports:
- **SOC 2**: Automated backups with encryption
- **PCI DSS**: Encrypted storage of financial data
- **GDPR**: Data recovery procedures
- **HIPAA**: Secure backup and restore

## Future Enhancements

- [ ] Incremental backups using WAL archiving
- [ ] Direct S3 upload without filesystem mount
- [ ] Backup verification without full restore
- [ ] Parallel compression for large databases
- [ ] Backup replication to multiple locations
- [ ] Automated restore testing
