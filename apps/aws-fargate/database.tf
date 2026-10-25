resource "aws_db_subnet_group" "catalog" {
  name       = var.name
  subnet_ids = aws_subnet.private[*].id
}

# URL-safe characters only: the password appears verbatim in `CONTEXTFUL_CATALOG_URL`.
resource "random_password" "catalog" {
  length  = 40
  special = false
}

resource "aws_db_instance" "catalog" {
  identifier     = var.name
  engine         = "postgres"
  engine_version = var.db_engine_version
  instance_class = var.db_instance_class

  db_name  = "contextful"
  username = "contextful"
  password = random_password.catalog.result

  allocated_storage     = var.db_allocated_storage
  max_allocated_storage = var.db_max_allocated_storage
  storage_type          = "gp3"
  storage_encrypted     = true
  kms_key_id            = var.kms_key_arn

  db_subnet_group_name   = aws_db_subnet_group.catalog.name
  vpc_security_group_ids = [aws_security_group.database.id]
  publicly_accessible    = false
  multi_az               = var.db_multi_az
  ca_cert_identifier     = var.db_ca_cert_identifier

  backup_retention_period    = var.db_backup_retention_days
  copy_tags_to_snapshot      = true
  deletion_protection        = var.db_deletion_protection
  final_snapshot_identifier  = "${var.name}-final"
  auto_minor_version_upgrade = true
  apply_immediately          = false
}

locals {
  catalog_query = var.rds_ca_bundle_path == "" ? "sslmode=require" : "sslmode=require&sslrootcert=${var.rds_ca_bundle_path}"
  catalog_url = format(
    "postgres://%s:%s@%s:%d/%s?%s",
    aws_db_instance.catalog.username,
    random_password.catalog.result,
    coalesce(var.catalog_host, aws_db_instance.catalog.address),
    aws_db_instance.catalog.port,
    aws_db_instance.catalog.db_name,
    local.catalog_query,
  )
}
