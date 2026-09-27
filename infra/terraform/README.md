# Terraform infrastructure modules

This directory contains reusable Terraform modules for the Postgres, Redis, and application compute layers described in the repo's deployment and Docker topology.

The structure is intentionally split into reusable modules and environment-specific composition so the same definitions can be reused for development, staging, and production deployments without duplicating the resource logic.

## Layout

- `modules/postgres`: Postgres primary instance + read replicas
- `modules/redis`: ElastiCache Redis service
- `modules/app`: ECS/Fargate application layer behind a load balancer
- `environments/dev|staging|prod`: sample environment declarations using the modules

## Plan-only validation

The provider is configured with placeholder AWS credentials and `skip_credentials_validation = true` so `terraform plan` can succeed in CI without a real cloud account. This is for validation-only planning; actual environment provisioning still requires real cloud credentials and account bootstrap.

Example:

```bash
cd infra/terraform/environments/dev
terraform init -backend=false
terraform plan -var="environment=dev" -var="aws_region=us-east-1"
```

## Notes

- These modules are intentionally scoped to environment provisioning and do not include one-time account bootstrapping.
- Read replicas are optional and default to zero for dev; staging and prod can enable them.
- The application module expects a prebuilt container image and uses a health check against `/ready`.
