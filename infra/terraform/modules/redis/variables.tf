variable "name" {
  description = "Logical name for the Redis stack."
  type        = string
}

variable "environment" {
  description = "Deployment environment name."
  type        = string
}

variable "aws_region" {
  description = "AWS region for the Redis cluster."
  type        = string
  default     = "us-east-1"
}

variable "node_type" {
  description = "Redis node type."
  type        = string
  default     = "cache.t4g.micro"
}

variable "num_cache_nodes" {
  description = "Number of cache nodes."
  type        = number
  default     = 1
}

variable "engine_version" {
  description = "Redis engine version."
  type        = string
  default     = "7.1"
}

variable "parameter_group_name" {
  description = "Parameter group name for the Redis cluster."
  type        = string
  default     = "default.redis7"
}

variable "subnet_ids" {
  description = "Subnets for the replication group."
  type        = list(string)
  default     = ["subnet-example-a", "subnet-example-b"]
}

variable "security_group_ids" {
  description = "Security groups allowed to access Redis."
  type        = list(string)
  default     = []
}

variable "multi_az_enabled" {
  description = "Whether to enable Multi-AZ replication."
  type        = bool
  default     = false
}
