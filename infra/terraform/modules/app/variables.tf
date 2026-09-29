variable "name" {
  description = "Logical name for the application stack."
  type        = string
}

variable "environment" {
  description = "Deployment environment name."
  type        = string
}

variable "image" {
  description = "Container image for the application service."
  type        = string
}

variable "cpu" {
  description = "CPU units for the task."
  type        = number
  default     = 256
}

variable "memory" {
  description = "Memory in MiB for the task."
  type        = number
  default     = 512
}

variable "container_port" {
  description = "Container port exposed by the service."
  type        = number
  default     = 3000
}

variable "desired_count" {
  description = "Desired number of app tasks."
  type        = number
  default     = 1
}

variable "vpc_id" {
  description = "VPC for the application load balancer and tasks."
  type        = string
  default     = "vpc-example"
}

variable "subnet_ids" {
  description = "Subnets for the application service."
  type        = list(string)
  default     = ["subnet-example-a", "subnet-example-b"]
}

variable "db_url" {
  description = "Database connection string for the application."
  type        = string
  sensitive   = true
  default     = "postgres://synapse_app:change-me@db.example.com:5432/synapse"
}

variable "redis_url" {
  description = "Redis connection string for the application."
  type        = string
  sensitive   = true
  default     = "redis://redis.example.com:6379"
}

variable "health_check_path" {
  description = "The application health endpoint."
  type        = string
  default     = "/ready"
}
