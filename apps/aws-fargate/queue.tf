# The `queue` role of spec/targets/aws.toml (`topology.compose.queue-sqs`): queued `receive`
# batches and object-arrival keys travel through one SQS standard queue, which every task
# reaches as `CONTEXTFUL_QUEUE_URL` under the task role. A message received
# `queue_max_receive_count` times without a delete moves to the dead-letter queue.

locals {
  queue_kms   = var.kms_key_arn
  arrival_arn = var.object_arrival_bucket == null ? null : "arn:${local.partition}:s3:::${var.object_arrival_bucket}"
}

resource "aws_sqs_queue" "dead_letter" {
  count                     = var.queue_enabled ? 1 : 0
  name                      = "${var.name}-queue-dlq"
  message_retention_seconds = 1209600
  max_message_size          = 1048576
  sqs_managed_sse_enabled   = local.queue_kms == null ? true : null
  kms_master_key_id         = local.queue_kms
}

resource "aws_sqs_queue" "this" {
  count = var.queue_enabled ? 1 : 0
  name  = "${var.name}-queue"
  # The adapter asks the same 300 s on each receive (`topology.compose.queue-sqs-visibility`)
  # and long-polls 20 s on its first call (`topology.compose.queue-sqs-wait`).
  visibility_timeout_seconds = 300
  receive_wait_time_seconds  = 20
  # 1 MiB, SQS's cap; a larger `receive` body spills to the corpus bucket under
  # `<corpus_prefix>/queue/spill/` (`topology.compose.queue-sqs-cap`).
  max_message_size          = 1048576
  message_retention_seconds = var.queue_retention_seconds
  sqs_managed_sse_enabled   = local.queue_kms == null ? true : null
  kms_master_key_id         = local.queue_kms

  redrive_policy = jsonencode({
    deadLetterTargetArn = aws_sqs_queue.dead_letter[0].arn
    maxReceiveCount     = var.queue_max_receive_count
  })
}

resource "aws_sqs_queue_redrive_allow_policy" "dead_letter" {
  count     = var.queue_enabled ? 1 : 0
  queue_url = aws_sqs_queue.dead_letter[0].id

  redrive_allow_policy = jsonencode({
    redrivePermission = "byQueue"
    sourceQueueArns   = [aws_sqs_queue.this[0].arn]
  })
}

data "aws_iam_policy_document" "queue" {
  count = var.queue_enabled ? 1 : 0

  statement {
    sid       = "DenyInsecureTransport"
    effect    = "Deny"
    actions   = ["sqs:*"]
    resources = [aws_sqs_queue.this[0].arn]

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }

  dynamic "statement" {
    for_each = local.arrival_arn == null ? [] : [local.arrival_arn]
    content {
      sid       = "ObjectArrivalNotifications"
      actions   = ["sqs:SendMessage"]
      resources = [aws_sqs_queue.this[0].arn]

      principals {
        type        = "Service"
        identifiers = ["s3.amazonaws.com"]
      }

      condition {
        test     = "ArnEquals"
        variable = "aws:SourceArn"
        values   = [statement.value]
      }

      condition {
        test     = "StringEquals"
        variable = "aws:SourceAccount"
        values   = [local.account]
      }
    }
  }
}

resource "aws_sqs_queue_policy" "this" {
  count     = var.queue_enabled ? 1 : 0
  queue_url = aws_sqs_queue.this[0].id
  policy    = data.aws_iam_policy_document.queue[0].json
}

# An object created under the arrival prefix enqueues its key; the source's scheduled listing
# stays the backstop for a missed event (`run.land.object-arrival-wake`). The notification
# configuration replaces every other notification on that bucket.
resource "aws_s3_bucket_notification" "arrival" {
  count  = var.queue_enabled && var.object_arrival_bucket != null ? 1 : 0
  bucket = var.object_arrival_bucket

  queue {
    id            = "${var.name}-object-arrival"
    queue_arn     = aws_sqs_queue.this[0].arn
    events        = ["s3:ObjectCreated:*"]
    filter_prefix = var.object_arrival_prefix
    filter_suffix = var.object_arrival_suffix
  }

  depends_on = [aws_sqs_queue_policy.this]
}

# The task role receives, sends and deletes on the one queue; the dead-letter queue stays
# with the operator.
data "aws_iam_policy_document" "task_queue" {
  count = var.queue_enabled ? 1 : 0

  statement {
    sid       = "QueuePort"
    actions   = ["sqs:SendMessage", "sqs:ReceiveMessage", "sqs:DeleteMessage"]
    resources = [aws_sqs_queue.this[0].arn]
  }
}

resource "aws_iam_role_policy" "task_queue" {
  count  = var.queue_enabled ? 1 : 0
  name   = "queue"
  role   = aws_iam_role.task.id
  policy = data.aws_iam_policy_document.task_queue[0].json
}
