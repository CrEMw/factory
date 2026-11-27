# The tools image: the PR gate (.tekton/pr.yaml) runs in it. The build context is the repo root:
#   docker build --platform linux/arm64 -f tools/Dockerfile -t tools .
# arm64 builds only on arm64: under QEMU the aspire-managed server does not see its parent process
# and shuts itself down, restore fails (checked on amd64). To check on amd64: --platform linux/amd64.
# Inside is everything .tools/aspire and tools/chart.sh call, and what the gate checks the services with:
# node and npm, pnpm, cargo with rustfmt and clippy (version from services/rust-toolchain.toml), yq, helm, git. No .NET SDK.
# bwrap is not needed: the image has no system dotnet, and the .tools/aspire wrapper does not call it in that case.
# There is no "# syntax=" line on purpose: it pulls docker/dockerfile by a floating tag, and the built-in
# BuildKit 0.26 frontend is enough.
FROM docker.io/library/node:26.13.1-trixie-slim@sha256:e6bedef217677b276c591795e169840edd9096dbf13f29df8ef8d1856b8820bc AS node

# The same Rust image as in services/Dockerfile: cargo and the linker are already inside.
FROM docker.io/library/rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7
ARG TARGETARCH
# git for the diff in the gate; libicu is needed by the .NET runtime embedded in the Aspire CLI.
# shortcut: apt package versions are not pinned, only the base image digest and the BuildKit cache hold them.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git libicu76 openssh-client perl \
    && rm -rf /var/lib/apt/lists/*
# rustfmt and clippy for the rust-lint part of tools/check.sh: the slim Rust image lacks them. Same version
# as the image toolchain (services/rust-toolchain.toml); rustup cannot install them as uid 1000.
RUN rustup component add rustfmt clippy
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
    && npm install --global pnpm@12.20.0 \
    && npm cache clean --force
# yq, helm and gitleaks (the leak scan of the published tree): release files verified by sha256. curl and
# sha256sum rather than ADD --checksum: Dagger (dagger/src/index.ts) builds this file too and refuses ADD of a URL
# with a checksum. amd64 is for CI on GitHub runners and for checking the image on a developer machine.
RUN set -eu; \
    case "$TARGETARCH" in \
      arm64) yq=89f69310b61b68d1245065a1241d1fc843f7a36cd6103ffd77ad987613bd1b67 \
             helm=31c5794dd55c66a51e6b7d2e2ac7a114ae8b1de41ff1d9ba51748ac973b06a08 \
             gl=e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080 glarch=arm64 ;; \
      amd64) yq=129534601871538efe0ba4c5e62a34fb1d5f469bcb975c4fddeb467d82974973 \
             helm=86584a54def73570558f66f5111cc53dfed56689637ae32c1201205d494f54fb \
             gl=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb glarch=x64 ;; \
      *) echo "unsupported TARGETARCH $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    d="$(mktemp -d)"; \
    curl -fsSLo "$d/yq" "https://github.com/mikefarah/yq/releases/download/v4.54.3/yq_linux_${TARGETARCH}"; \
    curl -fsSLo "$d/helm.tar.gz" "https://get.helm.sh/helm-v4.3.0-linux-${TARGETARCH}.tar.gz"; \
    curl -fsSLo "$d/gitleaks.tar.gz" \
      "https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_${glarch}.tar.gz"; \
    printf '%s  %s\n' "$yq" "$d/yq" "$helm" "$d/helm.tar.gz" "$gl" "$d/gitleaks.tar.gz" | sha256sum -c -; \
    install -m 0755 "$d/yq" /usr/local/bin/yq; \
    tar -xzf "$d/helm.tar.gz" -C /usr/local/bin --strip-components=1 "linux-${TARGETARCH}/helm"; \
    tar -xzf "$d/gitleaks.tar.gz" -C /usr/local/bin gitleaks; \
    rm -rf "$d"

# Not root; numeric so that the kubelet can check runAsNonRoot. git and .NET need the passwd entry.
RUN useradd --uid 1000 --user-group --create-home ci
USER 1000:1000
# tools/chart.sh takes yq from here rather than via mise; .tools/aspire keeps the Aspire home here, not in .tools/home.
ENV YQ=yq \
    ASPIRE_HOME_DIR=/home/ci/aspire

# The Aspire home is filled at image build time: the CLI bundle (about 340 MB) and the NuGet packages (about 470 MB)
# are not downloaded in the gate. Only the version pins come from the repo, the image has no sources: the warm-up
# directory is removed in the same layer. The apphost.mts stub lets restore find the AppHost via
# aspire.config.json. If a PR changes the Aspire versions, the gate downloads the difference itself; a person then
# writes the new tools digest. safe.directory: git does not argue about the directory owner
# when the repo is mounted into the container from outside.
RUN --mount=type=bind,source=.tools/package.json,target=/in/.tools/package.json \
    --mount=type=bind,source=.tools/package-lock.json,target=/in/.tools/package-lock.json \
    --mount=type=bind,source=.tools/aspire,target=/in/.tools/aspire \
    --mount=type=bind,source=aspire.config.json,target=/in/aspire.config.json \
    --mount=type=bind,source=apphost/package.json,target=/in/apphost/package.json \
    --mount=type=bind,source=apphost/package-lock.json,target=/in/apphost/package-lock.json \
    --mount=type=bind,source=apphost/tsconfig.apphost.json,target=/in/apphost/tsconfig.apphost.json \
    cp -r /in /tmp/warm && cd /tmp/warm \
    && npm ci --prefix .tools && (cd apphost && npm ci) \
    && : > apphost/apphost.mts \
    && .tools/aspire restore --non-interactive \
    && cd / && rm -rf /tmp/warm /tmp/NuGetScratch* \
    && git config --global --add safe.directory '*'
WORKDIR /workspace
