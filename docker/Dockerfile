# Instruction counting needs cachegrind, and there is no usable valgrind on
# Apple Silicon or Windows. Running tak in here gets you the deterministic
# metric on any host.
#
# The build context is the repository root, not this directory, because the
# build stage copies Cargo.toml and src/:
#
#   docker build -f docker/Dockerfile -t tak .
#   docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/w" -w /w \
#     tak run --bench startup -- ./mycli --help
#
# `--user` matters: tak spawns the benchmark subject, so whatever you are
# measuring inherits the container's privileges over the mounted working tree.
# Mapping to your own uid keeps a faulty subject from writing root-owned files
# into your checkout.
#
# Measuring a *host-built* binary works too, as long as the container's glibc is
# at least as new as the one it was linked against.

FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --bin tak

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      valgrind curl tar gzip xz-utils unzip git ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/tak /usr/local/bin/tak

# Default to an unprivileged user so the benchmark subject is not root out of
# the box. Override with `--user` to match your own uid when writing to a mount.
RUN useradd --create-home --uid 1000 tak
USER tak

ENTRYPOINT ["tak"]
