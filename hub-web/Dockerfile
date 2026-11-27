# syntax=docker/dockerfile:1
FROM docker.io/library/node:26.13.1-alpine3.24@sha256:6bd9ab43c0ec0190ca7b2d193906cb8b7edf6e7f4afa43ddeccd6452710bffb5 AS build
WORKDIR /src
# corepack is not shipped with Node 25+, so pnpm is installed explicitly, the same version as in package.json.
RUN npm install --global pnpm@12.20.0
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml* ./
RUN pnpm install --frozen-lockfile
COPY . .
# After the build only dependencies stay in node_modules (currently just @opentelemetry/api).
RUN pnpm run build && pnpm install --prod --frozen-lockfile

# build/ from adapter-node bundles everything except dependencies from package.json: SvelteKit keeps
# @opentelemetry/api external so that it and the SDK share one instance (https://svelte.dev/docs/kit/observability).
FROM docker.io/library/node:26.13.1-alpine3.24@sha256:6bd9ab43c0ec0190ca7b2d193906cb8b7edf6e7f4afa43ddeccd6452710bffb5
WORKDIR /app
COPY --from=build /src/build ./build
COPY --from=build /src/node_modules ./node_modules
COPY --from=build /src/package.json ./
# node in the image is uid 1000; numeric so that kubelet can check runAsNonRoot.
USER 1000:1000
ENTRYPOINT ["node", "build/index.js"]
