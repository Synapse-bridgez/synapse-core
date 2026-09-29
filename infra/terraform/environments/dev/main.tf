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

  name        = "synapse-dev"
  environment = "dev"
  aws_region  = var.aws_region
  vpc_id      = "vpc-example"
  subnet_ids  = ["subnet-example-a", "subnet-example-b"]

  db_name        = "synapse"
  db_username    = "synapse_admin"
  db_password    = "change-me-please"
  instance_class = "db.t4g.micro"
  read_replica_count = 0
}

module "redis" {
  source = "../../modules/redis"

  name        = "synapse-dev"
  environment = "dev"
  aws_region  = var.aws_region
  node_type   = "cache.t4g.micro"
  num_cache_nodes = 1
  subnet_ids  = ["subnet-example-a", "subnet-example-b"]
  multi_az_enabled = false
}

module "app" {
  source = "../../modules/app"

  name        = "synapse-dev"
  environment = "dev"
  image       = "ghcr.io/example/synapse-core:dev"
  desired_count = 1
  vpc_id      = "vpc-example"
  subnet_ids  = ["subnet-example-a", "subnet-example-b"]
  db_url      = module.postgres.database_url
  redis_url   = module.redis.url
}
