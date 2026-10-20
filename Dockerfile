# Multi-stage build: the console is compiled into the binary, so stage 1 builds
# the frontend and stage 2 is just the Rust build plus a tiny runtime image.
FROM node:22-alpine AS console
WORKDIR /src/frontend
COPY frontend/package*.json ./
RUN npm ci --no-audit --no-fund
COPY frontend/ ./
RUN npm run build

FROM rust:1.85-bookworm AS build
WORKDIR /src
# aws-lc-rs needs cmake; ring/aws-lc also want a C toolchain (present here).
RUN apt-get update && apt-get install -y --no-install-recommends cmake && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY --from=console /src/frontend/dist ./frontend/dist
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/hejiu45-oss /usr/local/bin/hejiu45-oss
USER nonroot
EXPOSE 9000
# Data lives on a volume; the master key is in the environment or the volume.
VOLUME ["/var/lib/hejiu45-oss"]
ENTRYPOINT ["/usr/local/bin/hejiu45-oss"]
CMD ["--data-dir", "/var/lib/hejiu45-oss", "--listen", "0.0.0.0:9000", "--tls", "reverse-proxy"]
