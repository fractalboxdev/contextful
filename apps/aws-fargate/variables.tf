variable "region" {
  description = "AWS region every resource lives in. The `[sync] endpoint` of the project image is `s3://<region>`."
  type        = string
}

variable "name" {
  description = "Name prefix for every resource, and the Secrets Manager name prefix of this deployment's secrets."
  type        = string
  default     = "contextful"

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{1,22}$", var.name))
    error_message = "name is 2 to 23 lowercase letters, digits or hyphens, starting with a letter."
  }
}

variable "shape" {
  description = "Target shape: `scheduled-fargate` starts `pipeline serve --cycle` from EventBridge Scheduler; `fargate-daemon` keeps `pipeline serve` in one always-on service."
  type        = string
  default     = "scheduled-fargate"

  validation {
    condition     = contains(["scheduled-fargate", "fargate-daemon"], var.shape)
    error_message = "shape is `scheduled-fargate` or `fargate-daemon`."
  }
}

variable "tags" {
  description = "Tags added to every resource."
  type        = map(string)
  default     = {}
}

# ------------------------------------------------------------------ network

variable "vpc_cidr" {
  description = "CIDR block of the VPC. Private subnets take the low /20s, public subnets the high ones."
  type        = string
  default     = "10.40.0.0/16"
}

variable "az_count" {
  description = "Availability zones spanned by the subnets, the database subnet group and the load balancer."
  type        = number
  default     = 2

  validation {
    condition     = var.az_count >= 2 && var.az_count <= 4
    error_message = "az_count is 2 to 4; RDS and the load balancer require two zones."
  }
}

variable "enable_nat" {
  description = "Create one NAT gateway for outbound internet access from the private subnets. Sources outside AWS need it; the AWS services the tasks use resolve through VPC endpoints without it."
  type        = bool
  default     = false
}

# ------------------------------------------------------------------ load balancer

variable "alb_internal" {
  description = "Place the load balancer in the private subnets, reachable only inside the VPC and its peers."
  type        = bool
  default     = false
}

variable "certificate_arn" {
  description = "ACM certificate ARN for the HTTPS listener. Required for an internet-facing load balancer; without it an internal load balancer listens on HTTP port 80."
  type        = string
  default     = null
}

variable "ingress_cidrs" {
  description = "CIDR blocks admitted to the load balancer listener."
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "alb_idle_timeout" {
  description = "Seconds the load balancer holds an idle connection open."
  type        = number
  default     = 120
}

# ------------------------------------------------------------------ image

variable "image_uri" {
  description = "Full URI of the project image. Null creates an ECR repository and runs `<repository>:<image_tag>`."
  type        = string
  default     = null
}

variable "image_tag" {
  description = "Tag of the project image in the created ECR repository."
  type        = string
  default     = "latest"
}

variable "image_repository_arn" {
  description = "ECR repository ARN that `image_uri` names, scoping the execution role's pull permission. Null with `image_uri` set grants pull on every repository in the account."
  type        = string
  default     = null
}

variable "cpu_architecture" {
  description = "Fargate CPU architecture of the project image."
  type        = string
  default     = "X86_64"

  validation {
    condition     = contains(["X86_64", "ARM64"], var.cpu_architecture)
    error_message = "cpu_architecture is X86_64 or ARM64."
  }
}

# ------------------------------------------------------------------ contextful

variable "project" {
  description = "Project name passed as `--project`. Null lets the nearest `contextful.toml` in the image's `/data` name it."
  type        = string
  default     = null
}

variable "recovery_id" {
  description = "Whole-state recovery identity, `CONTEXTFUL_RECOVERY_ID`. `contextful storage initialize` binds the database to it once."
  type        = string
}

variable "audience" {
  description = "Audience every credential presented to `contextful serve` names, `CONTEXTFUL_AUDIENCE`."
  type        = string
}

variable "issuer_public_keys" {
  description = "Comma-separated issuer key pins, `CONTEXTFUL_ISSUER_PUBKEY`, for `serve` and for pulled control receipts. Null omits the variable."
  type        = string
  default     = null
}

variable "serve_port" {
  description = "Container port `contextful serve --http 0.0.0.0:<port>` binds."
  type        = number
  default     = 8080
}

variable "serve_max_in_flight" {
  description = "`--max-in-flight` of `contextful serve`: requests in flight per task before a request answers 503."
  type        = number
  default     = 64
}

variable "serve_result_cache_bytes" {
  description = "`--result-cache-bytes` of `contextful serve`. Null caches no results."
  type        = number
  default     = null
}

variable "serve_count" {
  description = "Tasks the `contextful serve` service keeps running."
  type        = number
  default     = 2
}

variable "serve_cpu" {
  description = "Fargate CPU units of a `contextful serve` task."
  type        = number
  default     = 1024
}

variable "serve_memory" {
  description = "Fargate memory in MiB of a `contextful serve` task."
  type        = number
  default     = 4096
}

variable "pipeline_cpu" {
  description = "Fargate CPU units of the `pipeline serve` task."
  type        = number
  default     = 2048
}

variable "pipeline_memory" {
  description = "Fargate memory in MiB of the `pipeline serve` task."
  type        = number
  default     = 8192
}

variable "ephemeral_storage_gib" {
  description = "Fargate ephemeral storage in GiB per task. It holds the pulled snapshot set and run working files."
  type        = number
  default     = 50

  validation {
    condition     = var.ephemeral_storage_gib >= 21 && var.ephemeral_storage_gib <= 200
    error_message = "ephemeral_storage_gib is 21 to 200."
  }
}

variable "cycle_schedule" {
  description = "EventBridge Scheduler expression starting `pipeline serve --cycle` under `scheduled-fargate`."
  type        = string
  default     = "rate(5 minutes)"
}

variable "environment" {
  description = "Extra plain environment variables for every Contextful container."
  type        = map(string)
  default     = {}
}

variable "secrets" {
  description = "Extra environment variables injected from Secrets Manager into every Contextful container, as name => secret ARN (optionally `<arn>:<json-key>::`). The execution role reads exactly these."
  type        = map(string)
  default     = {}
}

variable "sync_credentials" {
  description = "How `[sync]` signs bucket requests: `role` signs with the task role (`[sync] credentials = \"role\"`); `keys` injects an access key pair from the `<name>/sync-keys` secret as `CONTEXTFUL_SYNC_ACCESS_KEY_ID` and `CONTEXTFUL_SYNC_SECRET_ACCESS_KEY`."
  type        = string
  default     = "role"

  validation {
    condition     = contains(["role", "keys"], var.sync_credentials)
    error_message = "sync_credentials is `role` or `keys`."
  }
}

variable "log_retention_days" {
  description = "CloudWatch Logs retention of the task log group."
  type        = number
  default     = 30
}

# ------------------------------------------------------------------ storage

variable "corpus_bucket_name" {
  description = "Name of the S3 corpus bucket. Null derives a unique name from `name`."
  type        = string
  default     = null
}

variable "corpus_prefix" {
  description = "Key prefix of the corpus inside the bucket, passed as `CONTEXTFUL_SYNC_PREFIX` for `[sync] prefix_from`. The task role reaches only keys under it."
  type        = string
  default     = "contextful"

  validation {
    condition     = can(regex("^[A-Za-z0-9][A-Za-z0-9._-]*(/[A-Za-z0-9._-]+)*$", var.corpus_prefix))
    error_message = "corpus_prefix is a non-empty key prefix without leading or trailing slashes."
  }
}

variable "noncurrent_version_days" {
  description = "Days a superseded or deleted corpus object version is kept before expiry."
  type        = number
  default     = 30
}

variable "kms_key_arn" {
  description = "Customer-managed KMS key ARN for the corpus bucket and the database. Null uses S3-managed keys and the RDS default key."
  type        = string
  default     = null
}

# ------------------------------------------------------------------ database

variable "db_engine_version" {
  description = "RDS for PostgreSQL engine version."
  type        = string
  default     = "17"
}

variable "db_instance_class" {
  description = "RDS instance class."
  type        = string
  default     = "db.t4g.medium"
}

variable "db_allocated_storage" {
  description = "Initial allocated storage in GiB."
  type        = number
  default     = 50
}

variable "db_max_allocated_storage" {
  description = "Storage autoscaling ceiling in GiB."
  type        = number
  default     = 200
}

variable "db_multi_az" {
  description = "Run a synchronous standby in a second availability zone."
  type        = bool
  default     = false
}

variable "db_backup_retention_days" {
  description = "Automated backup retention in days."
  type        = number
  default     = 7
}

variable "db_deletion_protection" {
  description = "Refuse deletion of the database instance."
  type        = bool
  default     = true
}

variable "db_ca_cert_identifier" {
  description = "RDS certificate authority signing the server certificate. The bundle at `rds_ca_bundle_path` holds its root."
  type        = string
  default     = "rds-ca-rsa2048-g1"
}

variable "rds_ca_bundle_path" {
  description = "Path of the RDS CA bundle inside the project image, set as `sslrootcert` in `CONTEXTFUL_CATALOG_URL`. Empty omits `sslrootcert`, for a catalog host whose certificate chains to a WebPKI root such as RDS Proxy."
  type        = string
  default     = "/etc/contextful/rds-global-bundle.pem"
}

variable "catalog_host" {
  description = "Host in `CONTEXTFUL_CATALOG_URL`. Null uses the database instance address; an RDS Proxy endpoint replaces it."
  type        = string
  default     = null
}
