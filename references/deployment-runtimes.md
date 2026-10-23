# Deployment runtimes

## Docker Desktop networking

- **Citation** — Docker, [Networking on Docker Desktop](https://docs.docker.com/desktop/features/networking/).
- **Priority** — must-read.
- **Informs** — `topology.deploy`.
- **Question** — Where do Linux containers execute on macOS, and how does a published port reach the host?

Docker Desktop places the Docker Engine in a Linux VM and forwards published ports to the host. The topology's macOS Docker example binds the Rust listener to the container's interfaces and publishes its port on macOS loopback. The shared project directory crosses Docker's file-sharing boundary.

Docker's [networking how-tos](https://docs.docker.com/desktop/features/networking/networking-how-tos/) define `host.docker.internal` for a container reaching a service on its host. The topology uses that name for Mac-hosted source and model endpoints.

## Multi-platform images

- **Citation** — Docker, [Multi-platform builds](https://docs.docker.com/build/building/multi-platform/).
- **Priority** — must-read.
- **Informs** — `topology.deploy`, `topology.package`.
- **Question** — Which image architecture matches the deployment CPU, and when does execution require emulation?

Linux containers share a Linux kernel and require a matching CPU architecture or emulation. The repository Dockerfile selects its builder and runtime platform through `PLATFORM` and verifies `RUST_TARGET` against the builder's Rust host. The topology's build examples pass both arguments alongside Docker's `--platform` selection.
