# Instruction counting needs cachegrind, and there is no usable valgrind on
# Apple Silicon or Windows. Running tak in here gets you the deterministic
# metric on any host — the binary is measured inside the container, so the
# numbers are comparable to what Linux CI produces.
#
#   docker build -t tak docker/
#   docker run --rm -v "$PWD:/w" -w /w tak run --bench startup -- ./mycli --help
#
# Note that measuring a *host-built* binary works too, as long as the container's
# glibc is at least as new as the one it was linked against.

FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends valgrind git ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/tak /usr/local/bin/tak
ENTRYPOINT ["tak"]
