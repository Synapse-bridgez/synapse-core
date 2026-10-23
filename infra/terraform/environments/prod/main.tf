terraform {
  required_version = ">= 1.6.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}

provider "aws" {
  region                      = var.aws_region
  access_key                  = var.aws_access_key
  secret_key                  = var.aws_secret_key
  skip_credentials_validation = true
  skip_requesting_account_id  = true
  skip_metadata_api_check     = true
}

module "postgres" {
  source = "../../modules/postgres"

  name               = "synapse-prod"
  environment        = "prod"
  aws_region         = var.aws_region
  vpc_id             = "vpc-example"
  subnet_ids         = ["subnet-example-a", "subnet-example-b"]
  instance_class     = "db.t4g.medium"
  read_replica_count = 2
  multi_az           = true
  deletion_protection = true
}

module "redis" {
  source = "../../modules/redis"

  name            = "synapse-prod"
  environment     = "prod"
  aws_region      = var.aws_region
  node_type       = "cache.t4g.medium"
  num_cache_nodes = 3
  subnet_ids      = ["subnet-example-a", "subnet-example-b"]
  multi_az_enabled = true
}

module "app" {
  source = "../../modules/app"

  name         = "synapse-prod"
  environment  = "prod"
  image        = "ghcr.io/example/synapse-core:prod"
  desired_count = 3
  vpc_id       = "vpc-example"
  subnet_ids   = ["subnet-example-a", "subnet-example-b"]
  db_url       = module.postgres.database_url
  redis_url    = module.redis.url
}

# ── Swap Engine Services — Issue #1490 ────────────────────────────────────────
# Worker, scheduler, and migration job for Phase 2 swap engine.
# Image digest must be pinned before deployment (never a floating tag).
module "swap_engine" {
  source = "../../modules/swap_engine"

  name             = "synapse-prod"
  environment      = "prod"
  aws_region       = var.aws_region
  vpc_id           = "vpc-example"
  subnet_ids       = ["subnet-example-a", "subnet-example-b"]
  ecs_cluster_arn  = "arn:aws:ecs:us-east-1:123456789012:cluster/synapse-prod-cluster"
  ecs_cluster_name = "synapse-prod-cluster"

  # Pin image digest in production — never use :latest or :prod tags directly
  image        = "ghcr.io/synapse-bridgez/synapse-core"
  image_digest = "sha256:replace-with-pinned-digest-before-deploy"

  execution_role_arn = "arn:aws:iam::123456789012:role/ecsExecutionRole"
  task_role_arn      = "arn:aws:iam::123456789012:role/ecsTaskRole"

  # Secrets Manager ARNs — no plaintext values here
  db_url_secret_arn          = "arn:aws:secretsmanager:us-east-1:123456789012:secret:synapse-prod/db-url"
  redis_url_secret_arn       = "arn:aws:secretsmanager:us-east-1:123456789012:secret:synapse-prod/redis-url"
  vault_role_id_secret_arn   = "arn:aws:secretsmanager:us-east-1:123456789012:secret:synapse-prod/vault-role-id"
  vault_secret_id_secret_arn = "arn:aws:secretsmanager:us-east-1:123456789012:secret:synapse-prod/vault-secret-id"
  webhook_secret_arn         = "arn:aws:secretsmanager:us-east-1:123456789012:secret:synapse-prod/webhook-secret"

  horizon_url  = "https://horizon.stellar.org"
  sdex_enabled = true
  amm_enabled  = true
  rust_log     = "warn"

  # Resource sizing — from docs/load-test-results.md p99 baseline + 40% headroom
  worker_cpu           = 512
  worker_memory        = 512
  worker_desired_count = 3
  worker_min_count     = 2
  worker_max_count     = 20
  scheduler_cpu        = 256
  scheduler_memory     = 256
  log_retention_days   = 90
}
