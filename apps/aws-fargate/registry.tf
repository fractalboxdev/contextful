resource "aws_ecr_repository" "project" {
  count                = var.image_uri == null ? 1 : 0
  name                 = var.name
  image_tag_mutability = "IMMUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = var.kms_key_arn == null ? "AES256" : "KMS"
    kms_key         = var.kms_key_arn
  }
}

resource "aws_ecr_lifecycle_policy" "project" {
  count      = var.image_uri == null ? 1 : 0
  repository = aws_ecr_repository.project[0].name

  policy = jsonencode({
    rules = [{
      rulePriority = 1
      description  = "Keep the 20 most recent images"
      selection = {
        tagStatus   = "any"
        countType   = "imageCountMoreThan"
        countNumber = 20
      }
      action = { type = "expire" }
    }]
  })
}

locals {
  image          = var.image_uri != null ? var.image_uri : "${aws_ecr_repository.project[0].repository_url}:${var.image_tag}"
  repository_arn = var.image_uri == null ? aws_ecr_repository.project[0].arn : var.image_repository_arn
}
