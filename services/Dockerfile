# syntax=docker/dockerfile:1
# One Dockerfile for the whole workspace: both Aspire (publish) and Tekton (buildctl) read it.
# The service is chosen by an argument: --build-arg PACKAGE=hub
FROM docker.io/library/rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7 AS build
ARG PACKAGE=hub
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --package "$PACKAGE" \
    && cp "target/release/$PACKAGE" /app \
    && mkdir /seed && if [ -d "$PACKAGE/seed" ]; then cp -a "$PACKAGE/seed/." /seed/; fi

FROM docker.io/library/debian:13.7-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2
# Root certificates for outgoing HTTPS: slim has none, take them from the build image without apt.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /app /app
# Installation seeds (services/hub/seed, src/seed.rs): the directory travels in the image only until step 5.4 of the
# factory plan, where it comes from a ConfigMap in the installation values. HUB_SEED_DIR, HUB_SEED_REQUIRED and
# HUB_DEFAULT_DOCS_REPO are set by the installation values (hub.env), not here.
COPY --from=build /seed /seed
USER 65532:65532
ENTRYPOINT ["/app"]
