output "endpoint" {
  description = "Load balancer DNS name; point a DNS record for the certificate's name at it."
  value       = aws_lb.serve.dns_name
}

output "image" {
  description = "Image every task runs."
  value       = local.image
}

output "ecr_repository_url" {
  description = "Created ECR repository the project image is pushed to, or null when image_uri is set."
  value       = one(aws_ecr_repository.project[*].repository_url)
}

output "corpus_bucket" {
  description = "S3 corpus bucket, the `[sync] bucket` of the project image."
  value       = aws_s3_bucket.corpus.id
}

output "corpus_policy_arn" {
  description = "Managed policy granting the corpus prefix; attach it to the interim `keys` principal."
  value       = aws_iam_policy.corpus.arn
}

output "catalog_secret_arn" {
  description = "Secrets Manager secret holding CONTEXTFUL_CATALOG_URL."
  value       = aws_secretsmanager_secret.catalog_url.arn
}

output "sync_keys_secret_arn" {
  description = "Secret the operator fills with the interim access key pair under sync_credentials = \"keys\"."
  value       = one(aws_secretsmanager_secret.sync_keys[*].arn)
}

output "source_secret_prefix" {
  description = "Secrets Manager name prefix that `secret://<name>` references resolve under (CONTEXTFUL_AWS_SECRET_PREFIX)."
  value       = local.source_secret_prefix
}

output "cluster" {
  description = "ECS cluster name."
  value       = aws_ecs_cluster.this.name
}

output "pipeline_task_definition" {
  description = "Task definition of `pipeline serve`; a one-off `storage initialize` runs it with a command override."
  value       = aws_ecs_task_definition.pipeline.arn
}

output "task_network" {
  description = "awsvpc configuration for a one-off `aws ecs run-task`."
  value = {
    subnets         = aws_subnet.private[*].id
    security_groups = [aws_security_group.tasks.id]
  }
}

output "store_config" {
  description = "The store `config.toml` the project image carries at `/data/.contextful/context/<project>/config.toml`."
  value = templatefile("${path.module}/templates/config.toml.tftpl", {
    region      = var.region
    bucket      = aws_s3_bucket.corpus.id
    credentials = var.sync_credentials
  })
}

output "queue_url" {
  description = "SQS queue behind the `Queue` port, passed as CONTEXTFUL_QUEUE_URL, or null when queue_enabled is false."
  value       = one(aws_sqs_queue.this[*].url)
}

output "dead_letter_queue_url" {
  description = "Dead-letter queue holding messages received queue_max_receive_count times without a delete, or null when queue_enabled is false."
  value       = one(aws_sqs_queue.dead_letter[*].url)
}
