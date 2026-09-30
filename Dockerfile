FROM rust:1.98.1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY desktop/src-tauri/Cargo.toml ./desktop/src-tauri/Cargo.toml
COPY desktop/src-tauri/src ./desktop/src-tauri/src
COPY src ./src
COPY static ./static
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home assistant && mkdir /app /app/data && chown -R assistant:assistant /app
WORKDIR /app
COPY --from=build /build/target/release/personal-teams-assistant /usr/local/bin/personal-teams-assistant
USER 10001:10001
EXPOSE 3000
ENTRYPOINT ["personal-teams-assistant"]
CMD ["serve", "/app/config.toml"]
