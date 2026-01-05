FROM rust:latest as builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Create app directory
WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src
COPY public ./public

# Build application in release mode
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    tesseract-ocr \
    tesseract-ocr-san \
    poppler-utils \
    pdftk \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create app directory
WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/sanskrit-ocr /app/sanskrit-ocr

# Copy public files
COPY --from=builder /app/public /app/public

# Create temp directory for file processing
RUN mkdir -p /tmp

# Expose port
EXPOSE 8080

# Set environment variables
ENV RUST_LOG=info

# Run the application
CMD ["/app/sanskrit-ocr"]
