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
