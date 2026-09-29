resource "aws_db_subnet_group" "this" {
  name       = "${var.name}-subnet-group"
  subnet_ids = var.subnet_ids

  tags = {
    Name        = "${var.name}-subnet-group"
    Environment = var.environment
  }
}

resource "aws_security_group" "this" {
  name        = "${var.name}-postgres-sg"
  description = "Allow database traffic for ${var.name}"
  vpc_id      = var.vpc_id

  ingress {
    description = "Postgres access from trusted app subnets"
    from_port   = 5432
    to_port     = 5432
    protocol    = "tcp"
    cidr_blocks = ["10.0.0.0/8"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = {
    Name        = "${var.name}-postgres-sg"
    Environment = var.environment
  }
}

resource "aws_db_parameter_group" "this" {
  name   = "${var.name}-postgres-pg"
  family = "postgres16"

  parameter {
    name  = "rds.force_ssl"
    value = "1"
  }

  tags = {
    Name        = "${var.name}-postgres-pg"
    Environment = var.environment
  }
}

resource "aws_db_instance" "primary" {
  identifier                          = "${var.name}-primary"
  engine                              = "postgres"
  engine_version                      = var.engine_version
  instance_class                      = var.instance_class
  allocated_storage                   = var.allocated_storage
  storage_type                        = var.storage_type
  db_name                             = var.db_name
  username                            = var.db_username
  password                            = var.db_password
  parameter_group_name                = aws_db_parameter_group.this.name
  db_subnet_group_name                = aws_db_subnet_group.this.name
  vpc_security_group_ids              = [aws_security_group.this.id]
  multi_az                            = var.multi_az
  backup_retention_period             = var.backup_retention_days
  skip_final_snapshot                 = true
  publicly_accessible                 = false
  deletion_protection                 = var.deletion_protection
  performance_insights_enabled        = var.performance_insights_enabled
  performance_insights_retention_period = var.performance_insights_enabled ? 7 : null
  apply_immediately                   = true

  tags = {
    Name        = "${var.name}-primary"
    Environment = var.environment
  }
}

resource "aws_db_instance" "read_replicas" {
  count = var.read_replica_count

  identifier = "${var.name}-replica-${count.index + 1}"

  replicate_source_db = aws_db_instance.primary.identifier
  instance_class      = var.replica_instance_class != "" ? var.replica_instance_class : var.instance_class

  parameter_group_name   = aws_db_parameter_group.this.name
  db_subnet_group_name   = aws_db_subnet_group.this.name
  vpc_security_group_ids = [aws_security_group.this.id]
  skip_final_snapshot    = true
  publicly_accessible    = false
  apply_immediately      = true
  multi_az               = false

  tags = {
    Name        = "${var.name}-replica-${count.index + 1}"
    Environment = var.environment
  }
}
