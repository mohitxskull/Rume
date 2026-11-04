FROM ubuntu:24.04 AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    build-essential \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY rume/Cargo.toml rume/Cargo.toml
COPY rume-macros/Cargo.toml rume-macros/Cargo.toml
COPY rume-cli/Cargo.toml rume-cli/Cargo.toml
COPY workbench/multi-instance/Cargo.toml workbench/multi-instance/Cargo.toml
COPY workbench/simple-api/Cargo.toml workbench/simple-api/Cargo.toml
COPY workbench/modular-monolith/Cargo.toml workbench/modular-monolith/Cargo.toml
COPY rume rume
COPY rume-macros rume-macros
COPY rume-cli rume-cli
COPY workbench workbench
RUN cargo build --release -p workbench --bin workbench

FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /src/target/release/workbench /app/workbench
COPY workbench/multi-instance/config /app/config
COPY workbench/multi-instance/migrations /app/migrations
COPY workbench/multi-instance/sql /app/sql
COPY workbench/multi-instance/.env.example /app/.env

ENV HOST=0.0.0.0
ENV PORT=8080
EXPOSE 8080

ENTRYPOINT ["/app/workbench"]
