# The container image of one profile (`assurance.build.container-image`): `contextful-full`
# unless `--build-arg PROFILE=contextful-edge` or `contextful-control` selects another.
#
#   docker build --platform linux/amd64 -t contextful .
#   docker run --rm -v "$PWD:/data" contextful query "SELECT 1"

# The builder: Rust 1.97 on Alpine, whose native target is musl, so the binary links
# statically, the SQL engine's C++ runtime included.
FROM --platform=linux/amd64 rust:1.97-alpine@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900 AS build
ARG PROFILE=contextful-full
RUN apk add --no-cache build-base
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p contextful-cli --bin contextful \
      --no-default-features --features "$PROFILE" --target x86_64-unknown-linux-musl \
 && cargo build --release --locked -p contextful-ci \
 && ./target/release/contextful-ci footprint --profile "$PROFILE" target/x86_64-unknown-linux-musl/release/contextful \
 && install -D target/x86_64-unknown-linux-musl/release/contextful /out/contextful \
 && mkdir -p /out/data

# The runtime: no shell, no package manager, no C library; CA roots for bucket endpoints.
FROM --platform=linux/amd64 gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=build /out/contextful /usr/local/bin/contextful
COPY --from=build --chown=65532:65532 /out/data /data
USER 65532:65532
WORKDIR /data
# The project directory: its `contextful.toml` and the store root under `.contextful/`.
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/contextful"]
CMD ["--version"]
