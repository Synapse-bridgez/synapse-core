resource "aws_elasticache_subnet_group" "this" {
  name       = "${var.name}-redis-subnet-group"
  subnet_ids = var.subnet_ids
}

resource "aws_security_group" "this" {
  count       = length(var.security_group_ids) == 0 ? 1 : 0
  name        = "${var.name}-redis-sg"
  description = "Allow Redis traffic for ${var.name}"
  vpc_id      = "vpc-example"

  ingress {
    description = "Redis access"
    from_port   = 6379
    to_port     = 6379
    protocol    = "tcp"
    cidr_blocks = ["10.0.0.0/8"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_elasticache_replication_group" "this" {
  replication_group_id       = "${var.name}-redis"
  description                = "Redis for ${var.name}"
  node_type                  = var.node_type
  num_cache_clusters        = var.num_cache_nodes
  engine                    = "redis"
  engine_version            = var.engine_version
  parameter_group_name      = var.parameter_group_name
  port                     = 6379
  subnet_group_name        = aws_elasticache_subnet_group.this.name
  security_group_ids       = length(var.security_group_ids) > 0 ? var.security_group_ids : [aws_security_group.this[0].id]
  multi_az_enabled          = var.multi_az_enabled
  automatic_failover_enabled = var.multi_az_enabled && var.num_cache_nodes > 1

  tags = {
    Name        = "${var.name}-redis"
    Environment = var.environment
  }
}
