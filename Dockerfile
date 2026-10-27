# Static musl build of the server (Alpine), run from distroless as a non-root user.
#   docker build -t erpcons-office-server:<git-sha> .
FROM rust:1.99.0-alpine3.22 AS build
RUN apk add --no-cache gcc musl-dev
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p erpcons-office-server \
 && install -D target/release/erpcons-office-server /out/erpcons-office-server \
 && mkdir -p /out/data

FROM gcr.io/distroless/static-debian13:nonroot
COPY --from=build /out/erpcons-office-server /usr/local/bin/erpcons-office-server
# Owned by the runtime user, so a fresh named volume mounted here starts out writable.
COPY --from=build --chown=65532:65532 /out/data /data
USER 65532:65532
# Inside the container only; compose publishes the port on the host's 127.0.0.1.
ENV ERPCONS_BIND=0.0.0.0:8710 \
    ERPCONS_DATA_DIR=/data
EXPOSE 8710
ENTRYPOINT ["/usr/local/bin/erpcons-office-server"]
