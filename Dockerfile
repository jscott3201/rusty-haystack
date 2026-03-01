# Multi-stage build for haystack CLI + server
FROM rust:1.93-alpine AS builder

RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY haystack-core/ haystack-core/
COPY haystack-server/ haystack-server/
COPY haystack-client/ haystack-client/
COPY haystack-cli/ haystack-cli/

# Create a stub for rusty-haystack (Python bindings) so the workspace resolves
RUN mkdir -p rusty-haystack/src && \
    printf '[package]\nname = "rusty-haystack"\nversion = "0.1.0"\nedition = "2024"\n\n[lib]\nname = "rusty_haystack"\ncrate-type = ["cdylib"]\n\n[dependencies]\npyo3 = { version = "0.24", features = ["extension-module"] }\n' > rusty-haystack/Cargo.toml && \
    echo '' > rusty-haystack/src/lib.rs

RUN cargo build --release -p haystack-cli && \
    strip target/release/haystack

# Runtime stage
FROM alpine:3.21

RUN apk add --no-cache ca-certificates

COPY --from=builder /build/target/release/haystack /usr/local/bin/haystack

EXPOSE 8080

ENTRYPOINT ["haystack"]
CMD ["serve", "--port", "8080"]
