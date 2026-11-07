# syntax=docker/dockerfile:1
# Test-script / mock image. Python is fine for the TEST script only;
# the proxy itself is all Rust. No secrets, no broker access.
FROM python:3.13-slim
WORKDIR /app
COPY test-script/ /app/
USER 1000:1000
