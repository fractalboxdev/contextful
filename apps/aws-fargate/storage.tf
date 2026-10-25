resource "aws_s3_bucket" "corpus" {
  bucket        = var.corpus_bucket_name
  bucket_prefix = var.corpus_bucket_name == null ? "${var.name}-corpus-" : null
}

resource "aws_s3_bucket_ownership_controls" "corpus" {
  bucket = aws_s3_bucket.corpus.id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_public_access_block" "corpus" {
  bucket                  = aws_s3_bucket.corpus.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_versioning" "corpus" {
  bucket = aws_s3_bucket.corpus.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "corpus" {
  bucket = aws_s3_bucket.corpus.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = var.kms_key_arn == null ? "AES256" : "aws:kms"
      kms_master_key_id = var.kms_key_arn
    }
    bucket_key_enabled = var.kms_key_arn != null
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "corpus" {
  bucket = aws_s3_bucket.corpus.id

  rule {
    id     = "expire-noncurrent-versions"
    status = "Enabled"

    filter {}

    noncurrent_version_expiration {
      noncurrent_days = var.noncurrent_version_days
    }

    expiration {
      expired_object_delete_marker = true
    }
  }

  depends_on = [aws_s3_bucket_versioning.corpus]
}

data "aws_iam_policy_document" "corpus_bucket" {
  statement {
    sid     = "DenyInsecureTransport"
    effect  = "Deny"
    actions = ["s3:*"]
    resources = [
      aws_s3_bucket.corpus.arn,
      "${aws_s3_bucket.corpus.arn}/*",
    ]

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

  statement {
    sid     = "DenyTlsBelow12"
    effect  = "Deny"
    actions = ["s3:*"]
    resources = [
      aws_s3_bucket.corpus.arn,
      "${aws_s3_bucket.corpus.arn}/*",
    ]

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    condition {
      test     = "NumericLessThan"
      variable = "s3:TlsVersion"
      values   = ["1.2"]
    }
  }
}

resource "aws_s3_bucket_policy" "corpus" {
  bucket = aws_s3_bucket.corpus.id
  policy = data.aws_iam_policy_document.corpus_bucket.json

  depends_on = [aws_s3_bucket_public_access_block.corpus]
}
