# Variables for the swap_engine Terraform module — Issue #1490

variable "name" {
  description = "Logical name prefix for all resources (e.g. synapse-prod)."
  type        = string
}

variable "environment" {
  description = "Deployment environment (dev | staging | prod)."
  type        = string
  validation {
    condition     = contains(["dev", "staging", "prod"], var.environment)
    error_message = "environment must be one of: dev, staging, prod."
  }
}

variable "aws_region" {
  description = "AWS region."
  type        = string
  default     = "us-east-1"
}

variable "vpc_id" {
  description = "VPC ID for all resources."
  type        = string
}

variable "subnet_ids" {
  description = "Private subnet IDs for ECS tasks (no public IP)."
  type        = list(string)
}

variable "ecs_cluster_arn" {
  description = "ARN of the ECS cluster to deploy into."
  type        = string
}

variable "ecs_cluster_name" {
  description = "Name of the ECS cluster (used for autoscaling resource ID)."
  type        = string
}

# ── Image ─────────────────────────────────────────────────────────────────────

variable "image" {
  description = "Container image repository (without digest)."
  type        = string
}

variable "image_digest" {
  description = "Pinned image digest (sha256:...). Never use a floating tag in prod."
  type        = string
}

# ── IAM roles ─────────────────────────────────────────────────────────────────

variable "execution_role_arn" {
  description = "ECS task execution role ARN (for pulling images and reading secrets)."
  type        = string
}

variable "task_role_arn" {
  description = "ECS task role ARN (for application-level AWS API calls)."
  type        = string
}

# ── Secrets (ARNs only — no plaintext values) ─────────────────────────────────

variable "db_url_secret_arn" {
  description = "AWS Secrets Manager ARN for DATABASE_URL."
  type        = string
  sensitive   = true
}

variable "redis_url_secret_arn" {
  description = "AWS Secrets Manager ARN for REDIS_URL."
  type        = string
  sensitive   = true
}

variable "vault_role_id_secret_arn" {
  description = "AWS Secrets Manager ARN for VAULT_ROLE_ID."
  type        = string
  sensitive   = true
}

variable "vault_secret_id_secret_arn" {
  description = "AWS Secrets Manager ARN for VAULT_SECRET_ID."
  type        = string
  sensitive   = true
}

variable "webhook_secret_arn" {
  description = "AWS Secrets Manager ARN for ANCHOR_WEBHOOK_SECRET."
  type        = string
  sensitive   = true
}

# ── Application config ────────────────────────────────────────────────────────

variable "horizon_url" {
  description = "Stellar Horizon base URL."
  type        = string
  default     = "https://horizon.stellar.org"
}

variable "sdex_enabled" {
  description = "Enable the SDEX liquidity adapter."
  type        = bool
  default     = false
}

variable "amm_enabled" {
  description = "Enable the AMM liquidity adapter."
  type        = bool
  default     = false
}

variable "container_port" {
  description = "Port the application listens on for health probes."
  type        = number
  default     = 3000
}

variable "rust_log" {
  description = "RUST_LOG value for all swap engine containers."
  type        = string
  default     = "info"
}

variable "processor_workers" {
  description = "Number of parallel processor worker threads."
  type        = number
  default     = 4
}

variable "processor_batch_size" {
  description = "Transaction batch size for the processor."
  type        = number
  default     = 100
}

# ── Networking ────────────────────────────────────────────────────────────────

variable "db_cidr_blocks" {
  description = "CIDR blocks for PostgreSQL egress."
  type        = list(string)
  default     = ["10.0.0.0/8"]
}

variable "redis_cidr_blocks" {
  description = "CIDR blocks for Redis egress."
  type        = list(string)
  default     = ["10.0.0.0/8"]
}

variable "vault_cidr_blocks" {
  description = "CIDR blocks for Vault egress."
  type        = list(string)
  default     = ["10.0.0.0/8"]
}

# ── Resource sizing (from load-test results) ──────────────────────────────────
# Baseline from docs/load-test-results.md:
#   swap-worker:    p99 RSS ~180 MB, CPU ~0.4 vCPU under batch load.
#   swap-scheduler: idle ~50 MB, CPU ~0.05 vCPU.
# Limits add 40% headroom for burst.

variable "worker_cpu" {
  description = "CPU units for the swap-worker task (1 vCPU = 1024)."
  type        = number
  default     = 512 # 0.5 vCPU
}

variable "worker_memory" {
  description = "Memory in MiB for the swap-worker task."
  type        = number
  default     = 512
}

variable "scheduler_cpu" {
  description = "CPU units for the swap-scheduler task."
  type        = number
  default     = 256
}

variable "scheduler_memory" {
  description = "Memory in MiB for the swap-scheduler task."
  type        = number
  default     = 256
}

variable "worker_desired_count" {
  description = "Initial desired replica count for the swap-worker service."
  type        = number
  default     = 2
}

variable "worker_min_count" {
  description = "Autoscaling minimum for the swap-worker."
  type        = number
  default     = 1
}

variable "worker_max_count" {
  description = "Autoscaling maximum for the swap-worker."
  type        = number
  default     = 10
}

variable "log_retention_days" {
  description = "CloudWatch log retention in days."
  type        = number
  default     = 30
}
