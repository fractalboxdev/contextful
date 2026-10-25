locals {
  https         = var.certificate_arn != null
  listener_port = local.https ? 443 : 80
}

resource "aws_lb" "serve" {
  name                       = var.name
  load_balancer_type         = "application"
  internal                   = var.alb_internal
  subnets                    = var.alb_internal ? aws_subnet.private[*].id : aws_subnet.public[*].id
  security_groups            = [aws_security_group.alb.id]
  idle_timeout               = var.alb_idle_timeout
  drop_invalid_header_fields = true

  lifecycle {
    precondition {
      condition     = var.alb_internal || local.https
      error_message = "An internet-facing load balancer requires certificate_arn; bearer credentials never cross the internet in cleartext."
    }
  }
}

# `GET /health?ready=hydrated` admits no credential, answers 503 while snapshot-set
# hydration runs and 200 once it completes, so the target group routes no read to a
# warming task (`read.register.health-hydrated`).
resource "aws_lb_target_group" "serve" {
  name                 = "${var.name}-serve"
  port                 = var.serve_port
  protocol             = "HTTP"
  target_type          = "ip"
  vpc_id               = aws_vpc.this.id
  deregistration_delay = 30

  health_check {
    path                = "/health?ready=hydrated"
    matcher             = "200"
    interval            = 15
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }
}

resource "aws_lb_listener" "primary" {
  load_balancer_arn = aws_lb.serve.arn
  port              = local.listener_port
  protocol          = local.https ? "HTTPS" : "HTTP"
  ssl_policy        = local.https ? "ELBSecurityPolicy-TLS13-1-2-2021-06" : null
  certificate_arn   = var.certificate_arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.serve.arn
  }
}

resource "aws_lb_listener" "redirect" {
  count             = local.https ? 1 : 0
  load_balancer_arn = aws_lb.serve.arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type = "redirect"

    redirect {
      port        = "443"
      protocol    = "HTTPS"
      status_code = "HTTP_301"
    }
  }
}
