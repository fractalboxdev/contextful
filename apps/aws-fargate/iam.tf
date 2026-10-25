data "aws_iam_policy_document" "ecs_tasks_trust" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account]
    }

    condition {
      test     = "ArnLike"
      variable = "aws:SourceArn"
      values   = ["arn:${local.partition}:ecs:${var.region}:${local.account}:*"]
    }
  }
}

# ------------------------------------------------------------------ execution role
# The ECS agent's identity: pull the image, write logs, inject the secrets the task definition names.

locals {
  # A `valueFrom` may carry a `:<json-key>:<stage>:<version>` suffix; the policy names the secret ARN.
  injected_secret_arns = distinct(concat(
    [aws_secretsmanager_secret.catalog_url.arn],
    aws_secretsmanager_secret.sync_keys[*].arn,
    [for v in values(var.secrets) : join(":", slice(split(":", v), 0, 7))],
  ))
}

data "aws_iam_policy_document" "execution" {
  statement {
    sid       = "EcrAuthorization"
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"]
  }

  statement {
    sid = "EcrPull"
    actions = [
      "ecr:BatchCheckLayerAvailability",
      "ecr:BatchGetImage",
      "ecr:GetDownloadUrlForLayer",
    ]
    resources = [coalesce(local.repository_arn, "arn:${local.partition}:ecr:${var.region}:${local.account}:repository/*")]
  }

  statement {
    sid       = "Logs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${aws_cloudwatch_log_group.tasks.arn}:*"]
  }

  statement {
    sid       = "InjectedSecrets"
    actions   = ["secretsmanager:GetSecretValue"]
    resources = local.injected_secret_arns
  }
}

resource "aws_iam_role" "execution" {
  name               = "${var.name}-execution"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_trust.json
}

resource "aws_iam_role_policy" "execution" {
  name   = "execution"
  role   = aws_iam_role.execution.id
  policy = data.aws_iam_policy_document.execution.json
}

# ------------------------------------------------------------------ corpus policy
# A managed policy, so the interim `keys` principal holds exactly the task role's bucket reach.

data "aws_iam_policy_document" "corpus" {
  statement {
    sid       = "ListCorpusPrefix"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.corpus.arn]

    condition {
      test     = "StringLike"
      variable = "s3:prefix"
      values   = [var.corpus_prefix, "${var.corpus_prefix}/*"]
    }
  }

  statement {
    sid       = "ReadWriteCorpusObjects"
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.corpus.arn}/${var.corpus_prefix}/*"]
  }

  dynamic "statement" {
    for_each = var.kms_key_arn == null ? [] : [var.kms_key_arn]
    content {
      sid       = "CorpusKey"
      actions   = ["kms:Decrypt", "kms:GenerateDataKey"]
      resources = [statement.value]
    }
  }
}

resource "aws_iam_policy" "corpus" {
  name        = "${var.name}-corpus"
  description = "Read, write and list the Contextful corpus prefix of one bucket"
  policy      = data.aws_iam_policy_document.corpus.json
}

# ------------------------------------------------------------------ task role
# The Contextful process's identity: the corpus prefix and the source secret prefix, nothing else.

data "aws_iam_policy_document" "task" {
  statement {
    sid       = "SourceSecrets"
    actions   = ["secretsmanager:GetSecretValue"]
    resources = ["arn:${local.partition}:secretsmanager:${var.region}:${local.account}:secret:${local.source_secret_prefix}*"]
  }
}

resource "aws_iam_role" "task" {
  name               = "${var.name}-task"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_trust.json
}

resource "aws_iam_role_policy" "task" {
  name   = "source-secrets"
  role   = aws_iam_role.task.id
  policy = data.aws_iam_policy_document.task.json
}

resource "aws_iam_role_policy_attachment" "task_corpus" {
  role       = aws_iam_role.task.name
  policy_arn = aws_iam_policy.corpus.arn
}

# ------------------------------------------------------------------ scheduler role

data "aws_iam_policy_document" "scheduler_trust" {
  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["scheduler.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account]
    }
  }
}

data "aws_iam_policy_document" "scheduler" {
  statement {
    sid       = "RunCycleTask"
    actions   = ["ecs:RunTask"]
    resources = ["${aws_ecs_task_definition.pipeline.arn_without_revision}:*"]

    condition {
      test     = "ArnEquals"
      variable = "ecs:cluster"
      values   = [aws_ecs_cluster.this.arn]
    }
  }

  statement {
    sid       = "PassTaskRoles"
    actions   = ["iam:PassRole"]
    resources = [aws_iam_role.execution.arn, aws_iam_role.task.arn]

    condition {
      test     = "StringEquals"
      variable = "iam:PassedToService"
      values   = ["ecs-tasks.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "scheduler" {
  count              = local.scheduled ? 1 : 0
  name               = "${var.name}-scheduler"
  assume_role_policy = data.aws_iam_policy_document.scheduler_trust.json
}

resource "aws_iam_role_policy" "scheduler" {
  count  = local.scheduled ? 1 : 0
  name   = "run-cycle"
  role   = aws_iam_role.scheduler[0].id
  policy = data.aws_iam_policy_document.scheduler.json
}
