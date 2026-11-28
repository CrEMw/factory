# factory

A self-hosted ticket hub for teams that work with coding agents. One hub keeps the tickets of several projects, the
links between them, the design documents of your repositories and a mailbox through which agent sessions report to
each other, and it exposes all of that to agents over MCP.

## What it does

- **Tickets**: projects, tickets, statuses, assignees (people) and components, with a web UI and an HTTP API.
- **Graph**: tickets, projects and documents are nodes; links such as `blocks` and `part_of` are edges that are
  proposed and confirmed, never deleted (an edge is closed instead). The `/graph` page draws it in layers.
- **Document sync**: a repository sends a snapshot of its design documents to `POST /api/documents/sync`
  (`tools/doc-sync-client.mjs` is the client); the hub derives titles, roles and the links between documents and
  tickets. Documents are browsable under `/d/`.
- **Agent mailbox**: agents post short reports and read the messages of other sessions; the `/agents` page shows
  the feed and the mailbox keys.
- **MCP server**: hub-web serves MCP at `<origin>/mcp` (Streamable HTTP) with OAuth sign-in. Tools include
  `list_tickets`, `get_ticket`, `create_ticket`, `update_ticket`, `set_ticket_fields`, `link_tickets`, `unlink`,
  `get_links`, `get_graph`, `ready_tickets`, `list_projects`, `list_people`, `list_components`, `list_documents`,
  `get_document`, `ticket_stats`, `set_estimate`, `set_wsjf`, `get_plan`, `next_task`, `backlog` and `mailbox`.
- **Plan views**: estimates and WSJF per ticket, plan scopes, a critical chain with a fever chart per scope
  (`/plan`), the next task per person (`/next`) and a ranked backlog (`/backlog`).
- **UI language**: English by default, Russian with `HUB_LOCALE=ru`.

## Architecture

- `services/hub`: the Rust service. It owns the Postgres schema and the HTTP API; `hub migrate` applies the schema
  migrations and exits, plain `hub` serves the API.
- `hub-web`: the SvelteKit app. UI, sign-in (Better Auth), the MCP server; it calls `hub` with a caller secret.
- Postgres: one database. `hub` and `hub-web` use separate roles; the schema owner is used only by `hub migrate`.
- `apphost/apphost.mts`: the Aspire AppHost (TypeScript). It describes the service graph for the local run and is
  also the source of the Helm chart.
- `deploy/overlay`: what the AppHost cannot express (patches, extra templates, values defaults).
  `tools/chart.sh` merges it into the generated chart in `deploy/chart`.
- Both services send OpenTelemetry traces, metrics and logs over OTLP when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

## Quick start (local, with Aspire)

You need node and npm, Rust (the toolchain in `services/rust-toolchain.toml`), mise for
pnpm, and Docker. No .NET SDK is needed. If the machine has a system .NET in `/usr/share/dotnet`, you also need
`bwrap` (bubblewrap): the wrapper hides that .NET from Aspire.

Install once:

```sh
npm ci --prefix .tools          # the pinned Aspire CLI
(cd apphost && npm ci)          # AppHost dependencies
.tools/aspire restore           # Aspire integration packages, generates apphost/.aspire/modules
(cd hub-web && mise exec pnpm@12.20.0 -- pnpm install --frozen-lockfile)
```

Always run Aspire through `.tools/aspire`. The wrapper keeps everything Aspire writes to the home directory in
`.tools/home` (or in `ASPIRE_HOME_DIR`) instead of your real home.

Run:

```sh
mise exec pnpm@12.20.0 -- .tools/aspire run
```

This starts Postgres in a container, `hub-migrate` (`hub migrate`), then `hub` and `hub-web`, plus the Aspire
dashboard; Aspire prints the addresses. The local run uses the demo account `demo`, the time zone `UTC`, the example
seed in `examples/seed` and a fixed development sign-in (`AUTH_DEV_LOGIN`). To run in the background use
`.tools/aspire run --detach`, then `.tools/aspire describe` and `.tools/aspire stop`.

## Configuration

Environment variables of `hub` and `hub-web` and the Helm values that set them are described in
[docs/configuration.md](docs/configuration.md). The chart defaults are in `deploy/overlay/values.yaml`.

## Installing the Helm chart

The chart is named `factory` and lives in `deploy/chart`. It is generated: do not edit it by hand (see
[AGENTS.md](AGENTS.md)). The defaults in `deploy/overlay/values.yaml` are neutral; an installation passes its own
values file over them. Examples are in `examples/values/`.

The cluster needs:

- CloudNativePG for the database (`hub-db`);
- External Secrets Operator and a secret store named by `secrets.store.kind` and
  `secrets.store.name`; the keys it must hold are listed under `secrets.remoteKeys`;
- the barman-cloud CNPG plugin, only with `backup.enabled: true`;
- the Prometheus Operator (PodMonitor CRD), only with `db.podMonitor.enabled: true`.

Ingress through Tailscale (`ingress.tailscale`) and the Cloudflare tunnel connector (`ingress.cloudflared`) are
optional and off by default.

Values without a default must be set: `hub.instance.account` and `hub.instance.timezone`. Set `origin` to the public
address of hub-web, and the images:

```yaml
parameters:
  hub:
    hub_image: registry.example.com/factory/hub@sha256:...
  hub_web:
    hub_web_image: registry.example.com/factory/hub-web@sha256:...
origin: https://hub.example.com
hub:
  instance:
    account: acme
    timezone: UTC
```

Build the images from this repository:

```sh
docker build -t hub services
docker build -t hub-web hub-web
```

Render and check the chart with your values:

```sh
helm lint deploy/chart -f my-values.yaml
helm template factory deploy/chart -n factory -f my-values.yaml
```

The chart is written for Argo CD: sync waves order the secrets, the database, the migration Job `hub-db-migrate`
(an Argo CD Sync hook) and the services. Deploy it with an Argo CD Application that points at `deploy/chart` and
your values file.

## Database migrations

`hub migrate` applies the schema; the service itself only verifies it at startup. How the migrations, the baseline
and `hub migrate --adopt` work: [docs/migrations.md](docs/migrations.md).

## Development

`tools/check.sh` runs every check (Rust tests and lints, hub-web build and tests, node tests, chart drift, schema
checks). Rules for contributors and coding agents: [AGENTS.md](AGENTS.md).

## License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
