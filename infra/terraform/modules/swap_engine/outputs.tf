# Outputs for the swap_engine Terraform module — Issue #1490

output "swap_worker_service_name" {
  description = "ECS service name for the swap worker."
  value       = aws_ecs_service.swap_worker.name
}

output "swap_scheduler_service_name" {
  description = "ECS service name for the swap scheduler."
  value       = aws_ecs_service.swap_scheduler.name
}

output "swap_migrate_task_definition_arn" {
  description = "Task definition ARN for the migration job. Pass to `aws ecs run-task` in CI before updating services."
  value       = aws_ecs_task_definition.swap_migrate.arn
}

output "swap_worker_security_group_id" {
  description = "Security group ID for the swap worker (use in DB/Redis/Vault ingress rules)."
  value       = aws_security_group.swap_worker.id
}

output "swap_worker_log_group" {
  description = "CloudWatch log group name for the swap worker."
  value       = aws_cloudwatch_log_group.swap_worker.name
}

output "swap_scheduler_log_group" {
  description = "CloudWatch log group name for the swap scheduler."
  value       = aws_cloudwatch_log_group.swap_scheduler.name
}

output "swap_migrate_log_group" {
  description = "CloudWatch log group name for the migration job."
  value       = aws_cloudwatch_log_group.swap_migrate.name
}
