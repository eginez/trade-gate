# syntax=docker/dockerfile:1
# Test-script / mock image. Python is fine for the TEST script only;
# the proxy itself is all Rust. No secrets, no broker access.
# Alpine: the scripts are pure Python (stdlib only), so the musl
# base costs nothing and the image is a fraction of python:-slim.
FROM python:3.13-alpine
WORKDIR /app
COPY test-script/ /app/
USER 1000:1000
