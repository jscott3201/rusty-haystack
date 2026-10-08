# Multi-stage build for haystack CLI + server
FROM rust:1.99.0-alpine AS builder

RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY haystack-core/ haystack-core/
COPY haystack-app/ haystack-app/
COPY haystack-server/ haystack-server/
COPY haystack-client/ haystack-client/
COPY haystack-cli/ haystack-cli/

# Cargo resolves every workspace member even when building only the CLI. Keep
# the real manifests and package identities so --locked uses the same graph.
# These two members are not compiled in this image; only target stubs are needed.
COPY rusty-haystack/Cargo.toml rusty-haystack/
COPY demo/niagara_sample/niagara-rusty-scrape/Cargo.toml demo/niagara_sample/niagara-rusty-scrape/
RUN mkdir -p rusty-haystack/src demo/niagara_sample/niagara-rusty-scrape/src && \
    touch rusty-haystack/src/lib.rs && \
    printf 'fn main() {}\n' > demo/niagara_sample/niagara-rusty-scrape/src/main.rs

RUN cargo build --locked --release -p rusty-haystack-cli && \
    strip target/release/haystack

# Runtime stage
FROM alpine:3.21

RUN apk add --no-cache ca-certificates

COPY --from=builder /build/target/release/haystack /usr/local/bin/haystack

EXPOSE 8080

ENTRYPOINT ["haystack"]
CMD ["serve", "--port", "8080"]
