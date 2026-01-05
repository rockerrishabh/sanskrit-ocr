FROM rust:latest as builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY public ./public

RUN cargo build --release

# Runtime stage - use the same base as rust:latest
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y \
    tesseract-ocr \
    tesseract-ocr-san \
    poppler-utils \
    pdftk \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/sanskrit-ocr /app/sanskrit-ocr
COPY --from=builder /app/public /app/public

RUN mkdir -p /tmp

EXPOSE 8080
ENV RUST_LOG=info

CMD ["/app/sanskrit-ocr"]