# `scheduled-fargate`: each tick starts one `pipeline serve --cycle` task. The cycle takes
# the cadence lease, runs every due unit and exits; a tick that meets a held lease exits
# without dispatching.
resource "aws_scheduler_schedule" "cycle" {
  count               = local.scheduled ? 1 : 0
  name                = "${var.name}-cycle"
  schedule_expression = var.cycle_schedule
  state               = "ENABLED"

  flexible_time_window {
    mode = "OFF"
  }

  target {
    arn      = aws_ecs_cluster.this.arn
    role_arn = aws_iam_role.scheduler[0].arn

    ecs_parameters {
      task_definition_arn = aws_ecs_task_definition.pipeline.arn
      launch_type         = "FARGATE"
      platform_version    = "LATEST"
      task_count          = 1

      network_configuration {
        subnets          = aws_subnet.private[*].id
        security_groups  = [aws_security_group.tasks.id]
        assign_public_ip = false
      }
    }

    retry_policy {
      maximum_event_age_in_seconds = 300
      maximum_retry_attempts       = 2
    }
  }
}
