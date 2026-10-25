# The project image: the `contextful-full` image plus one project's declaration, its store
# configuration and the RDS CA bundle. Build it from the project directory:
#
#   docker build --platform linux/amd64 -f <repo>/apps/aws-fargate/project.Dockerfile \
#     --build-arg BASE=<contextful-full image> --build-arg PROJECT=<project> \
#     -t <repository>:<tag> .

ARG BASE
FROM ${BASE}
ARG PROJECT
COPY --chown=65532:65532 contextful.toml /data/contextful.toml
COPY --chown=65532:65532 config.toml /data/.contextful/context/${PROJECT}/config.toml
ADD --chmod=0444 https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem /etc/contextful/rds-global-bundle.pem
