# Swap Engine Worker — ECS Fargate service module
# Issue #1490: Infrastructure-as-Code for Swap Engine Services
#
# Manages:
#   • swap-worker  — long-running fill processor
#   • swap-scheduler — cron-based scheduled jobs
#   • swap-migrate — pre-rollout migration job (runs once per deploy)
#
# Secrets are injected via AWS Secrets Manager references only.
# No plaintext secrets appear in task definitions or this file.

terraform {
  required_version = ">= 1.6.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}

# ── CloudWatch log group ───────────────────────────────────────────────────────

resource "aws_cloudwatch_log_group" "swap_worker" {
  name              = "/ecs/${var.name}-swap-worker"
  retention_in_days = var.log_retention_days
  tags              = local.common_tags
}

resource "aws_cloudwatch_log_group" "swap_scheduler" {
  name              = "/ecs/${var.name}-swap-scheduler"
  retention_in_days = var.log_retention_days
  tags              = local.common_tags
}

resource "aws_cloudwatch_log_group" "swap_migrate" {
  name              = "/ecs/${var.name}-swap-migrate"
  retention_in_days = var.log_retention_days
  tags              = local.common_tags
}

# ── Security group ────────────────────────────────────────────────────────────

resource "aws_security_group" "swap_worker" {
  name        = "${var.name}-swap-worker-sg"
  description = "Swap worker egress to DB, Redis, Vault, and Horizon"
  vpc_id      = var.vpc_id

  # No inbound — workers are consumers, not servers
  egress {
    description = "PostgreSQL"
    from_port   = 5432
    to_port     = 5432
    protocol    = "tcp"
    cidr_blocks = var.db_cidr_blocks
  }

  egress {
    description = "Redis"
    from_port   = 6379
    to_port     = 6379
    protocol    = "tcp"
    cidr_blocks = var.redis_cidr_blocks
  }

  egress {
    description = "Vault"
    from_port   = 8200
    to_port     = 8200
    protocol    = "tcp"
    cidr_blocks = var.vault_cidr_blocks
  }

  egress {
    description = "HTTPS (Horizon + external)"
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    description = "DNS"
    from_port   = 53
    to_port     = 53
    protocol    = "udp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = merge(local.common_tags, { Name = "${var.name}-swap-worker-sg" })
}

# ── Shared environment block (no secrets — secrets use secretsFrom) ───────────

locals {
  common_tags = {
    Project     = "synapse"
    Component   = "swap-engine"
    Environment = var.environment
    ManagedBy   = "terraform"
  }

  swap_env = [
    { name = "SERVER_PORT",           value = tostring(var.container_port) },
    { name = "RUST_LOG",              value = var.rust_log },
    { name = "SDEX_ENABLED",          value = tostring(var.sdex_enabled) },
    { name = "AMM_ENABLED",           value = tostring(var.amm_enabled) },
    { name = "SDEX_BASE_URL",         value = var.horizon_url },
    { name = "AMM_BASE_URL",          value = var.horizon_url },
    { name = "SDEX_TIMEOUT_MS",       value = "5000" },
    { name = "AMM_TIMEOUT_MS",        value = "5000" },
    { name = "SDEX_MAX_RETRIES",      value = "3" },
    { name = "AMM_MAX_RETRIES",       value = "3" },
    { name = "PROCESSOR_WORKERS",     value = tostring(var.processor_workers) },
    { name = "PROCESSOR_BATCH_SIZE",  value = tostring(var.processor_batch_size) },
  ]

  # Secrets injected from AWS Secrets Manager — no plaintext values
  swap_secrets = [
    {
      name      = "DATABASE_URL"
      valueFrom = var.db_url_secret_arn
    },
    {
      name      = "REDIS_URL"
      valueFrom = var.redis_url_secret_arn
    },
    {
      name      = "VAULT_ROLE_ID"
      valueFrom = var.vault_role_id_secret_arn
    },
    {
      name      = "VAULT_SECRET_ID"
      valueFrom = var.vault_secret_id_secret_arn
    },
    {
      name      = "ANCHOR_WEBHOOK_SECRET"
      valueFrom = var.webhook_secret_arn
    },
  ]
}

# ── Swap Worker task definition ────────────────────────────────────────────────

resource "aws_ecs_task_definition" "swap_worker" {
  family                   = "${var.name}-swap-worker"
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = tostring(var.worker_cpu)
  memory                   = tostring(var.worker_memory)
  execution_role_arn       = var.execution_role_arn
  task_role_arn            = var.task_role_arn

  container_definitions = jsonencode([
    {
      name      = "swap-worker"
      image     = "${var.image}@${var.image_digest}"
      essential = true
      command   = ["/app/synapse-core", "--mode", "swap-worker"]

      environment = local.swap_env
      secrets     = local.swap_secrets

      # Health: poll the /ready endpoint (worker exposes it on a dedicated port)
      healthCheck = {
        command     = ["CMD-SHELL", "wget -qO- http://localhost:${var.container_port}/ready || exit 1"]
        interval    = 30
        timeout     = 5
        retries     = 3
        startPeriod = 30
      }

      # Resource limits sized from load-test results (docs/load-test-results.md)
      # Worker: p99 RSS ~180 MB, CPU ~0.4 vCPU under batch load.
      # Hard limits add ~40% headroom.
      linuxParameters = {
        initProcessEnabled = true
      }

      logConfiguration = {
        logDriver = "awslogs"
        options = {
          "awslogs-group"         = aws_cloudwatch_log_group.swap_worker.name
          "awslogs-region"        = var.aws_region
          "awslogs-stream-prefix" = "swap-worker"
        }
      }

      # Graceful drain: SIGTERM → 60 s drain window → SIGKILL
      stopTimeout = 60
    }
  ])

  tags = local.common_tags
}

resource "aws_ecs_service" "swap_worker" {
  name            = "${var.name}-swap-worker"
  cluster         = var.ecs_cluster_arn
  task_definition = aws_ecs_task_definition.swap_worker.arn
  desired_count   = var.worker_desired_count
  launch_type     = "FARGATE"

  network_configuration {
    subnets          = var.subnet_ids
    security_groups  = [aws_security_group.swap_worker.id]
    assign_public_ip = false
  }

  # Zero-downtime rolling deploy: keep min 50% healthy, allow 100% max
  deployment_minimum_healthy_percent = 50
  deployment_maximum_percent         = 200

  # Deregistration delay allows in-flight orders to complete before the old
  # task is killed; aligns with the 60 s stopTimeout above.
  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  lifecycle {
    ignore_changes = [desired_count] # allow autoscaling to manage this
  }

  tags = local.common_tags
}

# ── Swap Scheduler task definition ────────────────────────────────────────────
# Scheduled jobs: partition maintenance, lot expiry, P&L roll-up.

resource "aws_ecs_task_definition" "swap_scheduler" {
  family                   = "${var.name}-swap-scheduler"
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = tostring(var.scheduler_cpu)
  memory                   = tostring(var.scheduler_memory)
  execution_role_arn       = var.execution_role_arn
  task_role_arn            = var.task_role_arn

  container_definitions = jsonencode([
    {
      name      = "swap-scheduler"
      image     = "${var.image}@${var.image_digest}"
      essential = true
      command   = ["/app/synapse-core", "--mode", "scheduler"]

      environment = local.swap_env
      secrets     = local.swap_secrets

      healthCheck = {
        command     = ["CMD-SHELL", "wget -qO- http://localhost:${var.container_port}/live || exit 1"]
        interval    = 60
        timeout     = 5
        retries     = 3
        startPeriod = 30
      }

      logConfiguration = {
        logDriver = "awslogs"
        options = {
          "awslogs-group"         = aws_cloudwatch_log_group.swap_scheduler.name
          "awslogs-region"        = var.aws_region
          "awslogs-stream-prefix" = "swap-scheduler"
        }
      }

      stopTimeout = 30
    }
  ])

  tags = local.common_tags
}

resource "aws_ecs_service" "swap_scheduler" {
  name            = "${var.name}-swap-scheduler"
  cluster         = var.ecs_cluster_arn
  task_definition = aws_ecs_task_definition.swap_scheduler.arn
  desired_count   = 1 # singleton scheduler
  launch_type     = "FARGATE"

  network_configuration {
    subnets          = var.subnet_ids
    security_groups  = [aws_security_group.swap_worker.id]
    assign_public_ip = false
  }

  deployment_minimum_healthy_percent = 100
  deployment_maximum_percent         = 200

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  tags = local.common_tags
}

# ── Migration job ──────────────────────────────────────────────────────────────
# One-shot task: runs sqlx migrate run before the worker/scheduler services
# are updated.  Invoked by CI via `aws ecs run-task` before `aws ecs
# update-service`.  See docs/deployment.md §"Migration ordering".

resource "aws_ecs_task_definition" "swap_migrate" {
  family                   = "${var.name}-swap-migrate"
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = "256"
  memory                   = "512"
  execution_role_arn       = var.execution_role_arn
  task_role_arn            = var.task_role_arn

  container_definitions = jsonencode([
    {
      name      = "swap-migrate"
      image     = "${var.image}@${var.image_digest}"
      essential = true
      command   = ["/app/synapse-migrate"]

      environment = [
        { name = "RUST_LOG", value = "info" },
      ]
      secrets = [
        {
          name      = "DATABASE_URL"
          valueFrom = var.db_url_secret_arn
        }
      ]

      logConfiguration = {
        logDriver = "awslogs"
        options = {
          "awslogs-group"         = aws_cloudwatch_log_group.swap_migrate.name
          "awslogs-region"        = var.aws_region
          "awslogs-stream-prefix" = "swap-migrate"
        }
      }
    }
  ])

  tags = local.common_tags
}

# ── Autoscaling (worker only) ─────────────────────────────────────────────────

resource "aws_appautoscaling_target" "swap_worker" {
  max_capacity       = var.worker_max_count
  min_capacity       = var.worker_min_count
  resource_id        = "service/${var.ecs_cluster_name}/${aws_ecs_service.swap_worker.name}"
  scalable_dimension = "ecs:service:DesiredCount"
  service_namespace  = "ecs"
}

resource "aws_appautoscaling_policy" "swap_worker_cpu" {
  name               = "${var.name}-swap-worker-cpu-scaling"
  policy_type        = "TargetTrackingScaling"
  resource_id        = aws_appautoscaling_target.swap_worker.resource_id
  scalable_dimension = aws_appautoscaling_target.swap_worker.scalable_dimension
  service_namespace  = aws_appautoscaling_target.swap_worker.service_namespace

  target_tracking_scaling_policy_configuration {
    predefined_metric_specification {
      predefined_metric_type = "ECSServiceAverageCPUUtilization"
    }
    # Scale out at 70% CPU — sized from load-test results (docs/load-test-results.md)
    target_value       = 70.0
    scale_in_cooldown  = 120
    scale_out_cooldown = 60
  }
}
