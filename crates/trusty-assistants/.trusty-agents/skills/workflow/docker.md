---
name: docker
description: Multi-stage builds, non-root user, layer caching, healthchecks, volume patterns
tags: [docker, containers, devops, dockerfile, multi-stage, security]
---

# Docker Containerization Skill

## Multi-Stage Builds

Always use multi-stage builds to keep the runtime image small. The builder stage
installs tools and compiles; the runtime stage copies only the final artifact.

```dockerfile
# ── Stage 1: builder ──────────────────────────────────────────────────────
FROM python:3.11-slim AS builder

WORKDIR /build

# Install build dependencies only in the builder stage
RUN pip install --upgrade pip
COPY requirements.txt .
RUN pip install --no-cache-dir --prefix=/install -r requirements.txt

# ── Stage 2: runtime ──────────────────────────────────────────────────────
FROM python:3.11-slim AS runtime

# Copy only the installed packages, not pip or build tools
COPY --from=builder /install /usr/local

WORKDIR /app
COPY src/ ./src/

# Non-root user (see below)
RUN adduser --disabled-password --gecos "" appuser
USER appuser

CMD ["python", "-m", "src.main"]
```

## Non-Root User

Never run the application as root. Create a dedicated user in the Dockerfile
and switch to it with `USER` before the final `CMD`/`ENTRYPOINT`.

```dockerfile
# Linux (Debian/Ubuntu-based images)
RUN adduser --disabled-password --gecos "" --uid 1001 appuser
USER appuser

# Alpine-based images
RUN addgroup -S appgroup && adduser -S appuser -G appgroup
USER appuser
```

If the application needs to write to a directory, `chown` it before switching:

```dockerfile
RUN mkdir -p /app/data && chown appuser:appuser /app/data
USER appuser
```

## Layer Caching Optimization

Docker caches each layer. Copy dependency manifests BEFORE source code so that
a source change does not invalidate the (expensive) dependency install layer.

```dockerfile
# Good — dependency layer is cached unless requirements.txt changes
COPY requirements.txt .
RUN pip install -r requirements.txt
COPY src/ ./src/

# Bad — any source change invalidates the pip install layer
COPY . .
RUN pip install -r requirements.txt
```

For Node.js:
```dockerfile
COPY package.json package-lock.json ./
RUN npm ci --omit=dev
COPY src/ ./src/
```

For Rust:
```dockerfile
COPY Cargo.toml Cargo.lock ./
# Dummy main to cache dependency compile
RUN mkdir src && echo "fn main(){}" > src/main.rs
RUN cargo build --release
RUN rm src/main.rs
COPY src/ ./src/
RUN cargo build --release
```

## HEALTHCHECK Patterns

Always add a HEALTHCHECK so container orchestrators (Docker Swarm, ECS, k8s)
can detect unhealthy containers and restart them.

```dockerfile
# HTTP health endpoint
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8000/health || exit 1

# TCP port check (when no HTTP health endpoint)
HEALTHCHECK --interval=10s --timeout=3s \
    CMD nc -z localhost 5432 || exit 1

# Custom script
HEALTHCHECK --interval=30s --timeout=10s \
    CMD python -c "import requests; requests.get('http://localhost:8000/health').raise_for_status()"
```

## Volume Mounts for Persistence

Use named volumes for data that must survive container restarts. Mount config
files as read-only bind mounts.

```yaml
# docker-compose.yml
services:
  app:
    image: myapp:latest
    volumes:
      - app_data:/app/data        # named volume for persistence
      - ./config:/app/config:ro   # read-only bind mount for config
    environment:
      - DATABASE_URL=postgresql://postgres:5432/app

volumes:
  app_data:
```

## .dockerignore

Always create a `.dockerignore` to prevent large or sensitive files from being
sent to the build context.

```dockerignore
# Dependencies installed during build
node_modules/
__pycache__/
*.pyc
.venv/
target/

# Development files
.git/
.env
.env.local
*.env.*
tests/
docs/
*.md

# Build artifacts
dist/
build/
*.log

# IDE
.vscode/
.idea/
```

## ENTRYPOINT vs CMD

- **ENTRYPOINT**: the fixed executable. Use for the primary application binary.
- **CMD**: default arguments to ENTRYPOINT, or the default command if no ENTRYPOINT.

```dockerfile
# Best pattern: ENTRYPOINT sets the executable, CMD sets default args
ENTRYPOINT ["python", "-m", "uvicorn"]
CMD ["src.main:app", "--host", "0.0.0.0", "--port", "8000"]
# Override CMD at runtime: docker run myapp src.main:app --workers 4
```

## ENV vs ARG

- **ENV**: sets an environment variable in the image at runtime. Visible to the
  running container.
- **ARG**: build-time variable, not present in the final image. Use for
  controlling build behavior (e.g. `ARG BUILD_ENV=production`).

```dockerfile
ARG PYTHON_VERSION=3.11
FROM python:${PYTHON_VERSION}-slim

ARG BUILD_ENV=production
RUN if [ "$BUILD_ENV" = "development" ]; then pip install -r requirements-dev.txt; fi

ENV PORT=8000
ENV APP_ENV=production
EXPOSE $PORT
```

## Minimal Base Images

Always use slim or minimal base images to reduce attack surface and image size.

| Use Case | Preferred Image |
|---|---|
| Python app | `python:3.11-slim` |
| Python + Alpine | `python:3.11-alpine` |
| Node.js | `node:20-slim` |
| Generic Linux | `debian:bookworm-slim` |
| Security-critical | `gcr.io/distroless/python3` |

Never use `python:3.11` (full Debian) when `python:3.11-slim` works.

## Anti-patterns

- Never run as root in production containers.
- Never copy `.env` files into the image — inject secrets via environment variables at runtime.
- Never use `latest` tags in production — pin to a specific version.
- Never install dev dependencies in the runtime stage.
- Never store secrets in `ARG` or `ENV` instructions — they appear in the image layer history.
