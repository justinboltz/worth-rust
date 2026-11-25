FROM rust:1.88-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .cargo .cargo
COPY src src
COPY migrations migrations
COPY static static
COPY templates templates
RUN cargo build --locked --release --bin worth

FROM debian:bookworm-slim
RUN apt-get update && apt-get install --no-install-recommends -y ca-certificates && rm -rf /var/lib/apt/lists/* && useradd --system --uid 10001 worth
WORKDIR /app
COPY --from=build /app/target/release/worth /usr/local/bin/worth
COPY static static
COPY templates templates
USER worth
ENV BIND_ADDRESS=0.0.0.0:3001 COOKIE_SECURE=true
EXPOSE 3001
CMD ["worth"]
