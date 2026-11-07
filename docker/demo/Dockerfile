# syntax=docker/dockerfile:1
# Demo image. Python is fine for the DEMO script only;
# the proxy itself is all Rust. No secrets, no broker access.
#
# Build context is the repo root (see docker-compose.yml).
# The demo runs against PROXY_BASE — in Compose that is the proxy
# service on the internal network. It never talks to the upstream
# directly, and it never sees a credential.
FROM python:3.13-slim
WORKDIR /app
COPY demo/ /app/
USER 1000:1000
ENTRYPOINT ["python", "/app/demo.py"]
