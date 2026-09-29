output "load_balancer_dns_name" {
  description = "DNS name of the application load balancer."
  value       = aws_lb.this.dns_name
}

output "service_name" {
  description = "Name of the ECS service."
  value       = aws_ecs_service.this.name
}

output "target_group_arn" {
  description = "Target group ARN for the app service."
  value       = aws_lb_target_group.this.arn
}
