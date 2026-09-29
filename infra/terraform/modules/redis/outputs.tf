output "primary_endpoint" {
  description = "Redis primary endpoint address."
  value       = aws_elasticache_replication_group.this.primary_endpoint_address
}

output "reader_endpoint" {
  description = "Redis reader endpoint address."
  value       = aws_elasticache_replication_group.this.reader_endpoint_address
}

output "port" {
  description = "Redis port."
  value       = aws_elasticache_replication_group.this.port
}

output "url" {
  description = "Redis connection URL."
  value       = "redis://${aws_elasticache_replication_group.this.primary_endpoint_address}:${aws_elasticache_replication_group.this.port}"
  sensitive   = true
}
