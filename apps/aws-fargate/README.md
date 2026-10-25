# AWS Fargate reference deployment

This OpenTofu configuration provisions the `scheduled-fargate` shape of [`spec/targets/aws.toml`](../../spec/targets/aws.toml): every process runs the native `contextful-full` executable in a Fargate task. EventBridge Scheduler starts `contextful pipeline serve --cycle` on `cycle_schedule`; the cycle takes the cadence lease, runs every due unit and exits. An ECS service keeps `serve_count` tasks of `contextful serve --http 0.0.0.0:<serve_port>` behind an Application Load Balancer. RDS Postgres holds the catalog and the engine journal under `CONTEXTFUL_STORAGE_PROFILE=postgres`; S3 holds corpus objects. `shape = "fargate-daemon"` replaces the schedule with one always-on `pipeline serve` task.

Every identifier in this directory is a placeholder or a variable. `terraform.tfvars.example` lists the values a deployment supplies; the real `terraform.tfvars`, the state and `.terraform/` stay untracked.

## Resources

| Resource | Configuration |
| --- | --- |
| VPC | `az_count` private subnets for tasks, the database and the interface endpoints; public subnets only for an internet-facing load balancer or `enable_nat` |
| VPC endpoints | S3 gateway on the private route table; interface endpoints for `ecr.api`, `ecr.dkr`, `secretsmanager`, `logs` and `sts`, answering HTTPS from the task security group |
| NAT gateway | One, when `enable_nat = true`; sources outside AWS need it, the AWS services above do not |
| RDS Postgres | `db_engine_version`, gp3 storage encrypted with the RDS key or `kms_key_arn`, private subnets, `publicly_accessible = false`, port 5432 open to the task security group alone |
| S3 corpus bucket | Public access blocked, bucket-owner-enforced ownership, versioning, SSE-S3 or SSE-KMS with `kms_key_arn`, a policy denying non-TLS and pre-TLS 1.2 requests, noncurrent versions expiring after `noncurrent_version_days` |
| ECR repository | Immutable tags, scan on push, 20 images retained; omitted when `image_uri` names an image elsewhere |
| ECS cluster | Container Insights on; the `pipeline` and `serve` task definitions share the image, the environment and the injected secrets |
| EventBridge Scheduler | One schedule whose ECS RunTask target starts the `pipeline` task definition in the private subnets, retried twice within 300 s |
| Application Load Balancer | HTTPS on 443 with `certificate_arn` and a TLS 1.3 policy, port 80 redirecting; an internal load balancer without a certificate listens on HTTP 80 |
| CloudWatch Logs | `/ecs/<name>`, streams prefixed `pipeline` and `serve`, kept `log_retention_days` |

An internet-facing load balancer without `certificate_arn` fails the plan: bearer credentials never cross the internet in cleartext.

## Roles

| Role | Grants |
| --- | --- |
| `<name>-execution` | `ecr:GetAuthorizationToken`; image pull on the one repository; log writes to the one log group; `secretsmanager:GetSecretValue` on the catalog URL secret, the interim key secret and each `secrets` entry |
| `<name>-task` | The `<name>-corpus` policy: `s3:ListBucket` limited to `corpus_prefix`, `s3:GetObject`, `s3:PutObject` and `s3:DeleteObject` under `<corpus_prefix>/`, and the KMS key when set; `secretsmanager:GetSecretValue` on names under `<name>/secrets/` |
| `<name>-scheduler` | `ecs:RunTask` on the `pipeline` task definition family in the one cluster; `iam:PassRole` on the execution and task roles, passed to `ecs-tasks.amazonaws.com` only |

Both task roles trust `ecs-tasks.amazonaws.com` for this account's ECS resources alone; the scheduler role trusts `scheduler.amazonaws.com` for this account alone. The sync adapter issues GET, PUT, DELETE and list requests, and no multipart upload, so the corpus policy grants nothing further.

## Project image

The `contextful-full` image has no shell, and a Fargate task mounts no project directory, so the project travels in an image built on it. [`project.Dockerfile`](project.Dockerfile) adds three files:

| Path | Content |
| --- | --- |
| `/data/contextful.toml` | The project declaration |
| `/data/.contextful/context/<project>/config.toml` | The store configuration; the `store_config` output renders it |
| `/etc/contextful/rds-global-bundle.pem` | The RDS CA bundle, read through `sslrootcert` |

```sh
docker build --platform linux/amd64 -t contextful-full .          # repository root, see Dockerfile
tofu output -raw store_config > <project-dir>/config.toml
docker build --platform linux/amd64 -f apps/aws-fargate/project.Dockerfile \
  --build-arg BASE=contextful-full --build-arg PROJECT=<project> \
  -t <ecr-repository-url>:<tag> <project-dir>
docker push <ecr-repository-url>:<tag>
```

`image_tag` selects the pushed tag; tags are immutable, so each release pushes a new one and a `tofu apply` with the new `image_tag` registers new task definition revisions.

The rendered store configuration carries no `[node]` block. Each task starts on a fresh disk and resolves its own node id under `store.lay-out.node-id-project`, so one task's pull never skips run parts another task wrote under a shared id.

```toml
[sync]
endpoint        = "s3://<region>"
bucket          = "<corpus-bucket>"
prefix_from     = "env:CONTEXTFUL_SYNC_PREFIX"
coordination    = "cas"
pull_before_run = true
push_after_run  = true
credentials     = "role"
```

`pull_before_run` and `push_after_run` are mandatory under `CONTEXTFUL_STORAGE_PROFILE=postgres` (`run.journal.container-profile`); a store configuration lacking either refuses with `StorageCapabilityMissing`.

## Environment

| Variable | Source | Containers |
| --- | --- | --- |
| `CONTEXTFUL_STORAGE_PROFILE` | `postgres` | both |
| `CONTEXTFUL_CATALOG_URL` | Secrets Manager `<name>/catalog-url`, injected by the execution role | both |
| `CONTEXTFUL_RECOVERY_ID` | `recovery_id` | both |
| `CONTEXTFUL_SYNC_PREFIX` | `corpus_prefix`, read by `[sync] prefix_from` | both |
| `CONTEXTFUL_AWS_SECRET_PREFIX` | `<name>/secrets/` | both |
| `CONTEXTFUL_ISSUER_PUBKEY` | `issuer_public_keys`, when set | both |
| `CONTEXTFUL_AUDIENCE` | `audience` | `serve` |
| `AWS_REGION` | `region` | both |
| `environment`, `secrets` | Variables, merged last | both |

`serve` takes `--max-in-flight` from `serve_max_in_flight` and `--result-cache-bytes` from `serve_result_cache_bytes`; `project` adds `--project` to both commands. A project whose applied control document schedules jobs that admit a credential needs `CONTEXTFUL_TOKEN` through `secrets`; without it, `pipeline serve` refuses with `FireCredentialMissing`. A source reaching a private VPC address lists that range in `CONTEXTFUL_EGRESS_PRIVATE_RANGES` through `environment` (`connector.attach.private-range-admitted`).

Every container runs under an init process, which reaps the child `contextful` processes a cycle dispatches, and receives `SIGTERM` 120 s before `SIGKILL`.

## Catalog TLS

`CONTEXTFUL_CATALOG_URL` is `postgres://contextful:<password>@<host>:5432/contextful?sslmode=require&sslrootcert=/etc/contextful/rds-global-bundle.pem`. Remote PostgreSQL requires verified TLS with hostname checking (`topology.coordinate.postgres-transport`). The RDS server certificate chains to the RDS CA named by `db_ca_cert_identifier`, which WebPKI omits; `sslrootcert` replaces the WebPKI roots for that connection with the bundle's (`topology.coordinate.postgres-ca-bundle`). An unreadable or empty bundle refuses with `DatabaseCaBundleUnreadable` before connecting. `rds_ca_bundle_path` moves the bundle; the `global-bundle.pem` URL in `project.Dockerfile` serves every commercial region.

RDS Proxy is the alternative for a build without `sslrootcert` support (see [`spec/status.md`](../../spec/status.md)). Its endpoint presents an ACM certificate chaining to Amazon Root CA 1, a WebPKI root. Set `catalog_host` to the proxy endpoint and `rds_ca_bundle_path = ""`. The engine takes only transaction-scoped advisory locks, so proxy session pinning costs multiplexing and nothing else.

`random_password` generates the database password, and the catalog URL secret holds it; the OpenTofu state therefore holds both. Keep state in an encrypted backend or under OpenTofu state encryption.

## Bucket credentials

`sync_credentials = "role"` renders `credentials = "role"` into the store configuration: requests sign with the task role's credentials from the ECS container credential endpoint, refreshed 300 s before expiry (`store.endpoint.role-credentials`). It requires a build that performs that clause; [`spec/status.md`](../../spec/status.md) records which do. An earlier build refuses the `credentials` key at parse.

`sync_credentials = "keys"` serves those builds. It creates the `<name>/sync-keys` secret, renders `access_key_id = "env://CONTEXTFUL_SYNC_ACCESS_KEY_ID"` and `secret_access_key = "env://CONTEXTFUL_SYNC_SECRET_ACCESS_KEY"`, and the task definitions inject both from the secret's JSON keys. Write the secret's value as `{"access_key_id": "...", "secret_access_key": "..."}` for a principal holding the `corpus_policy_arn` policy alone. Injected keys do not rotate in a running task; a rotation takes effect at the next task start.

## Secrets

`secret://<name>` references resolve through the Secrets Manager adapter as secret id `<name>/secrets/<reference>`, signed with the task role (`connector.resolve.aws-secrets-manager`). For a build without that adapter, inject each credential through `secrets` as an environment variable and reference it by `env://NAME`; the execution role reads exactly the ARNs listed there, and the value does not rotate in a running task.

## Health and warming

The target group checks `GET /health` every 15 s and expects `200`. `/health` admits no credential and answers `200` with the build identity and `"hydration": "warming"` or `"complete"` (`read.register.health`). A task reaches readiness within 8 s of a cold start, before hydration completes (`topology.publish-hostname.container-readiness`); until then every other route answers `503` with `Retry-After: 1` (`read.cache.warming-read`), and clients retry. `serve` pulls every bucket table before its first read (`store.pull.before-run`) and again every 30 s (`store.pull.serve-refresh`). A deployment registers replacement tasks before it drains the old ones, and the circuit breaker rolls back a revision whose tasks fail to start.

## First deployment

1. Copy `terraform.tfvars.example` to `terraform.tfvars` and replace every placeholder.
2. `tofu init`, then `tofu apply -target=aws_ecr_repository.project` when the configuration creates the repository.
3. Build and push the project image as above, then `tofu apply` with its `image_tag`.
4. Bind the empty catalog to `recovery_id` once, with the `pipeline` task definition and a command override:

   ```sh
   aws ecs run-task --cluster "$(tofu output -raw cluster)" \
     --task-definition "$(tofu output -raw pipeline_task_definition)" --launch-type FARGATE \
     --network-configuration "awsvpcConfiguration={subnets=[<subnet-ids>],securityGroups=[<task-security-group>],assignPublicIp=DISABLED}" \
     --overrides '{"containerOverrides":[{"name":"pipeline","command":["storage","initialize"]}]}'
   ```

   `task_network` prints the subnet and security group ids. A later `storage check` override validates the catalog, the recovery identity and the corpus.
5. Run `pipeline import`, then `pipeline apply`, as further command overrides. In a project with `[sync]`, each admits an admin capability and signs a control receipt (`surface.apply.synced-attestation`), so the override task carries that credential through `secrets`. The next schedule tick dispatches the applied pipelines.

## `fargate-daemon`

`shape = "fargate-daemon"` removes the schedule and the scheduler role and runs the `pipeline` task definition as a one-task service whose command is `contextful pipeline serve`: the in-process scheduler arms each unit under the cadence lease, for cadences below the one-minute floor of EventBridge Scheduler. A deployment stops the old task before starting its replacement, and the cadence lease fences any overlap. The `serve` service and every other resource stay identical.

## Validation

```sh
cd apps/aws-fargate
tofu fmt -check -recursive
tofu init -backend=false
tofu validate
```

`.terraform.lock.hcl` pins the provider versions and their checksums for Linux, macOS and Windows.
