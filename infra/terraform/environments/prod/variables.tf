variable "aws_region" {
  description = "AWS region to use for plan-only validation."
  type        = string
  default     = "us-east-1"
}

variable "aws_access_key" {
  description = "Plan-time AWS access key placeholder."
  type        = string
  default     = "test"
  sensitive   = true
}

variable "aws_secret_key" {
  description = "Plan-time AWS secret key placeholder."
  type        = string
  default     = "test"
  sensitive   = true
}
