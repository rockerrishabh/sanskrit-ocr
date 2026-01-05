# Sanskrit OCR - Docker Setup

## Quick Start

### Build the Docker image:
```bash
docker build -t sanskrit-ocr .
```

### Run the container:
```bash
docker run -p 8080:8080 sanskrit-ocr
```

### Access the application:
Open your browser and navigate to: http://localhost:8080

## Docker Compose (Optional)

Create a `docker-compose.yml` file:

```yaml
version: '3.8'

services:
  sanskrit-ocr:
    build: .
    ports:
      - "8080:8080"
    restart: unless-stopped
    volumes:
      - /tmp:/tmp
```

Run with:
```bash
docker-compose up -d
```

## Installed Dependencies

The Docker image includes:
- **Rust 1.75** (builder stage)
- **Debian Bookworm** (runtime)
- **Tesseract OCR** with Sanskrit language data (`tesseract-ocr-san`)
- **Poppler Utils** (for PDF to image conversion via `pdftoppm`)
- **pdftk** (for PDF splitting functionality)

## Environment Variables

- `RUST_LOG=info` - Set logging level (debug, info, warn, error)

## Notes

- The application uses `/tmp` for temporary file processing
- Port 8080 is exposed by default
- Multi-stage build keeps the final image size optimized
