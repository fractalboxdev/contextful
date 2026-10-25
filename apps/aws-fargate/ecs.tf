resource "aws_ecs_cluster" "this" {
  name = var.name

  setting {
    name  = "containerInsights"
    value = "enabled"
  }
}

resource "aws_cloudwatch_log_group" "tasks" {
  name              = "/ecs/${var.name}"
  retention_in_days = var.log_retention_days
}

locals {
  project_args = var.project == null ? [] : ["--project", var.project]

  environment = merge(
    {
      AWS_REGION                   = var.region
      CONTEXTFUL_STORAGE_PROFILE   = "postgres"
      CONTEXTFUL_RECOVERY_ID       = var.recovery_id
      CONTEXTFUL_SYNC_PREFIX       = var.corpus_prefix
      CONTEXTFUL_AWS_SECRET_PREFIX = local.source_secret_prefix
    },
    var.issuer_public_keys == null ? {} : { CONTEXTFUL_ISSUER_PUBKEY = var.issuer_public_keys },
    var.environment,
  )

  injected = merge(
    { CONTEXTFUL_CATALOG_URL = aws_secretsmanager_secret.catalog_url.arn },
    var.sync_credentials == "keys" ? {
      CONTEXTFUL_SYNC_ACCESS_KEY_ID     = "${aws_secretsmanager_secret.sync_keys[0].arn}:access_key_id::"
      CONTEXTFUL_SYNC_SECRET_ACCESS_KEY = "${aws_secretsmanager_secret.sync_keys[0].arn}:secret_access_key::"
    } : {},
    var.secrets,
  )

  pipeline_command = concat(["pipeline", "serve"], local.scheduled ? ["--cycle"] : [], local.project_args)

  serve_command = concat(
    ["serve", "--http", "0.0.0.0:${var.serve_port}", "--max-in-flight", tostring(var.serve_max_in_flight)],
    var.serve_result_cache_bytes == null ? [] : ["--result-cache-bytes", tostring(var.serve_result_cache_bytes)],
    local.project_args,
  )

  container = {
    image                  = local.image
    essential              = true
    readonlyRootFilesystem = false
    # ECS sends SIGTERM, waits this long, then SIGKILL; 120 s is the Fargate ceiling.
    stopTimeout = 120
    # An init process reaps the child `contextful` processes a cycle dispatches.
    linuxParameters = { initProcessEnabled = true }
    secrets         = [for k, v in local.injected : { name = k, valueFrom = v }]
  }
}

resource "aws_ecs_task_definition" "pipeline" {
  family                   = "${var.name}-pipeline"
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = var.pipeline_cpu
  memory                   = var.pipeline_memory
  execution_role_arn       = aws_iam_role.execution.arn
  task_role_arn            = aws_iam_role.task.arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = var.cpu_architecture
  }

  ephemeral_storage {
    size_in_gib = var.ephemeral_storage_gib
  }

  container_definitions = jsonencode([merge(local.container, {
    name        = "pipeline"
    command     = local.pipeline_command
    environment = [for k, v in local.environment : { name = k, value = v }]
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = aws_cloudwatch_log_group.tasks.name
        awslogs-region        = var.region
        awslogs-stream-prefix = "pipeline"
      }
    }
  })])
}

resource "aws_ecs_task_definition" "serve" {
  family                   = "${var.name}-serve"
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = var.serve_cpu
  memory                   = var.serve_memory
  execution_role_arn       = aws_iam_role.execution.arn
  task_role_arn            = aws_iam_role.task.arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = var.cpu_architecture
  }

  ephemeral_storage {
    size_in_gib = var.ephemeral_storage_gib
  }

  container_definitions = jsonencode([merge(local.container, {
    name    = "serve"
    command = local.serve_command
    # `serve` answers the connections it accepted within 20 s of SIGTERM, then exits
    # (`read.register.stop-drain`); 10 s more covers the exit before SIGKILL.
    stopTimeout  = 30
    environment  = [for k, v in merge(local.environment, { CONTEXTFUL_AUDIENCE = var.audience }) : { name = k, value = v }]
    portMappings = [{ containerPort = var.serve_port, protocol = "tcp" }]
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = aws_cloudwatch_log_group.tasks.name
        awslogs-region        = var.region
        awslogs-stream-prefix = "serve"
      }
    }
  })])
}

resource "aws_ecs_service" "serve" {
  name                              = "${var.name}-serve"
  cluster                           = aws_ecs_cluster.this.id
  task_definition                   = aws_ecs_task_definition.serve.arn
  desired_count                     = var.serve_count
  launch_type                       = "FARGATE"
  platform_version                  = "LATEST"
  health_check_grace_period_seconds = var.serve_hydration_grace_seconds
  propagate_tags                    = "SERVICE"

  network_configuration {
    subnets          = aws_subnet.private[*].id
    security_groups  = [aws_security_group.tasks.id]
    assign_public_ip = false
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.serve.arn
    container_name   = "serve"
    container_port   = var.serve_port
  }

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  depends_on = [aws_lb_listener.primary]
}

# `fargate-daemon`: one always-on `pipeline serve` holding the cadence lease. Replacement
# stops the old task before the new one starts.
resource "aws_ecs_service" "pipeline" {
  count                              = local.scheduled ? 0 : 1
  name                               = "${var.name}-pipeline"
  cluster                            = aws_ecs_cluster.this.id
  task_definition                    = aws_ecs_task_definition.pipeline.arn
  desired_count                      = 1
  launch_type                        = "FARGATE"
  platform_version                   = "LATEST"
  deployment_minimum_healthy_percent = 0
  deployment_maximum_percent         = 100
  propagate_tags                     = "SERVICE"

  network_configuration {
    subnets          = aws_subnet.private[*].id
    security_groups  = [aws_security_group.tasks.id]
    assign_public_ip = false
  }
}
