resource "aws_secretsmanager_secret" "catalog_url" {
  name        = "${var.name}/catalog-url"
  description = "CONTEXTFUL_CATALOG_URL: the Postgres connection string of the shared catalog and engine journal"
}

resource "aws_secretsmanager_secret_version" "catalog_url" {
  secret_id     = aws_secretsmanager_secret.catalog_url.id
  secret_string = local.catalog_url
}

# Under `sync_credentials = "keys"` the operator writes the value:
# {"access_key_id": "...", "secret_access_key": "..."} of a principal holding the corpus policy.
resource "aws_secretsmanager_secret" "sync_keys" {
  count       = var.sync_credentials == "keys" ? 1 : 0
  name        = "${var.name}/sync-keys"
  description = "Interim [sync] access key pair for the corpus bucket"
}

locals {
  # `secret://<name>` references resolve under this prefix through the Secrets Manager adapter.
  source_secret_prefix = "${var.name}/secrets/"
}
