output "primary_instance_id" {
  description = "RDS instance identifier for the primary writer."
  value       = aws_db_instance.primary.id
}

output "primary_endpoint" {
  description = "DNS endpoint for the primary Postgres instance."
  value       = aws_db_instance.primary.address
}

output "primary_port" {
  description = "Port used by the primary Postgres instance."
  value       = aws_db_instance.primary.port
}

output "reader_endpoint" {
  description = "Endpoint to use for read-only traffic when replicas are enabled."
  value       = length(aws_db_instance.read_replicas) > 0 ? aws_db_instance.read_replicas[0].address : aws_db_instance.primary.address
}

output "database_name" {
  description = "Database name configured for the app."
  value       = aws_db_instance.primary.db_name
}

output "database_url" {
  description = "Connection string for the primary Postgres instance."
  value       = "postgres://${aws_db_instance.primary.username}@${aws_db_instance.primary.address}:${aws_db_instance.primary.port}/${aws_db_instance.primary.db_name}"
  sensitive   = true
}
