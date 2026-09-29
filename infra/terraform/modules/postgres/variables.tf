variable "name" {
  description = "Logical name for the Postgres stack."
  type        = string
}

variable "environment" {
  description = "Deployment environment name."
  type        = string
}

variable "aws_region" {
  description = "AWS region for the database resources."
  type        = string
  default     = "us-east-1"
}

variable "vpc_id" {
  description = "VPC that hosts the database security group and subnet group."
  type        = string
  default     = "vpc-example"
}

variable "subnet_ids" {
  description = "Subnet IDs that host the Postgres instance."
  type        = list(string)
  default     = ["subnet-example-a", "subnet-example-b"]
}

variable "instance_class" {
  description = "Compute size for the primary instance."
  type        = string
  default     = "db.t4g.micro"
}

variable "replica_instance_class" {
  description = "Compute size for read replicas. Defaults to the primary size when left blank."
  type        = string
  default     = ""
}

variable "allocated_storage" {
  description = "Primary storage allocation in GiB."
  type        = number
  default     = 20
}

variable "storage_type" {
  description = "Storage type for the instance."
  type        = string
  default     = "gp3"
}

variable "db_name" {
  description = "Application database name."
  type        = string
  default     = "synapse"
}

variable "db_username" {
  description = "Postgres admin username."
  type        = string
  default     = "synapse_admin"
}

variable "db_password" {
  description = "Postgres admin password."
  type        = string
  sensitive   = true
  default     = "change-me-please"
}

variable "engine_version" {
  description = "Postgres engine version."
  type        = string
  default     = "16.4"
}

variable "multi_az" {
  description = "Whether to enable highly available multi-AZ deployment."
  type        = bool
  default     = false
}

variable "backup_retention_days" {
  description = "Days to retain automated backups."
  type        = number
  default     = 7
}

variable "deletion_protection" {
  description = "Whether to protect the instance from accidental deletion."
  type        = bool
  default     = false
}

variable "read_replica_count" {
  description = "Number of read replicas to create."
  type        = number
  default     = 0
}

variable "performance_insights_enabled" {
  description = "Enable Performance Insights."
  type        = bool
  default     = false
}
