// Service graph of the hub. TypeScript AppHost: https://aspire.dev/app-host/typescript-apphost/
// What is missing here (limits, nodeSelector, secretKeyRef, rollout strategy, tls.hosts): TypeScript SDK 13.6.5 cannot
// express it, so deploy/overlay adds these fields through tools/chart.sh. Details in docs/chart.md.
// Install values are read from .Values (defaults in deploy/overlay/values.yaml); '{{= X }}' yields an unquoted value.
import { fileURLToPath } from 'node:url';
import { CertificateTrustScope, createBuilder, OtlpProtocol, ProbeType } from './.aspire/modules/aspire.mjs';

const builder = await createBuilder();
const publish = await (await builder.executionContext()).isPublishMode();

// https://aspire.dev/integrations/compute/kubernetes/
const k8s = await builder.addKubernetesEnvironment('k8s');
await k8s.withHelm({
  configure: async (helm) => {
    // The chart name is part of the immutable Deployment selectors (app.kubernetes.io/name): an install that already
    // runs under another name keeps it with values nameOverride.
    await helm.withChartName('factory');
    await helm.withChartDescription('factory: hub, hub-web');
  },
});
await k8s.withDashboard({ enabled: false }); // the Aspire dashboard is not deployed to the cluster

// https://aspire.dev/integrations/frameworks/rust/rust-host/
// The app directory is the workspace root: it is also the Docker context and holds the Dockerfile.
const hub = await builder.addRustApp('hub', '../services');
await hub.withCargoPackage('hub');
await hub.withHttpEndpoint({ env: 'PORT' });
// Locally Aspire gives both services the dashboard's OTEL_EXPORTER_OTLP_ENDPOINT, but with the grpc protocol.
// Both SDKs are built with http/protobuf only, so the protocol is set explicitly. These variables do not reach the chart.
// https://aspire.dev/fundamentals/telemetry/
await hub.withOtlpExporter({ protocol: OtlpProtocol.HttpProtobuf });
// The dashboard address is https with the developer certificate. Without System, Aspire leaves hub the system root set
// and rustls answers UnknownIssuer (verified); System puts the system roots plus this certificate into SSL_CERT_FILE.
await hub.withCertificateTrustScope(CertificateTrustScope.System);
// Not both: withHttpProbe registers a health check with the same key itself, and aspire run fails.
if (publish) await hub.withHttpProbe(ProbeType.Readiness, { path: '/health' });
else await hub.withHttpHealthCheck({ path: '/health' });

// Local database: hub-web reads it too (Better Auth tables in the auth schema).
let localDb: Awaited<ReturnType<Awaited<ReturnType<typeof builder.addPostgres>>['addDatabase']>> | undefined;

if (publish) {
  // In the cluster CNPG runs the database. HUBDB_URI comes from the secret hub-db-hub (role hub, deploy/overlay); the hub pod
  // has no password of the owner app (secret hub-db-app): the Job hub-db-migrate below runs migrations.
  // addManifest accepts scalar fields only: https://aspire.dev/integrations/compute/kubernetes/#add-custom-kubernetes-manifests
  await hub.publishAsKubernetesService(async (resource) => {
    // Sync waves: database secrets -3, ObjectStore -3, Cluster -2, migration Job -1, hub-web 0, hub 1. This way, even in an
    // empty namespace (restore), the Job waits for a healthy database with the secret hub-db-app instead of starting first.
    await resource.addManifest('postgresql.cnpg.io/v1', 'Cluster', 'hub-db', {
      configure: async (m) => {
        await m.withAnnotation('argocd.argoproj.io/sync-options', 'Prune=false,Delete=false');
        await m.withAnnotation('argocd.argoproj.io/sync-wave', '-2');
        await m.withField('spec.instances', '{{= .Values.db.instances }}');
        await m.withField('spec.storage.size', '{{ .Values.db.storage.size }}');
        await m.withField('spec.storage.storageClass', '{{ .Values.db.storage.class }}');
        await m.withField('spec.resources.requests.cpu', '{{ .Values.db.resources.requests.cpu }}');
        await m.withField('spec.resources.requests.memory', '{{ .Values.db.resources.requests.memory }}');
        await m.withField('spec.resources.limits.cpu', '{{ .Values.db.resources.limits.cpu }}');
        await m.withField('spec.resources.limits.memory', '{{ .Values.db.resources.limits.memory }}');
      },
    });
    // Schema migrations: a Job with the same hub image and the argument migrate, as the owner app
    // (HUBDB_MIGRATE_URI from the CNPG secret hub-db-app). Argo CD Sync-phase hook in wave -1: runs on every sync
    // (a rerun changes nothing) after a healthy hub-db (wave -2) and before hub-web (wave 0) and hub (wave 1). Retries
    // (backoffLimit 6) outlast a database that does not accept connections yet. A failed Job stops the sync, and the old
    // pods keep running. The hook is not part of the Application resources, so the immutable Job template is no obstacle:
    // BeforeHookCreation deletes the previous Job before the new one, ttl removes a finished one after a day.
    // Container, nodeSelector and env are in deploy/overlay (addManifest cannot express arrays or keys with dots).
    await resource.addManifest('batch/v1', 'Job', 'hub-db-migrate', {
      configure: async (m) => {
        await m.withAnnotation('argocd.argoproj.io/hook', 'Sync');
        await m.withAnnotation('argocd.argoproj.io/hook-delete-policy', 'BeforeHookCreation');
        await m.withAnnotation('argocd.argoproj.io/sync-wave', '-1');
        await m.withField('spec.backoffLimit', 6);
        await m.withField('spec.activeDeadlineSeconds', 900);
        await m.withField('spec.ttlSecondsAfterFinished', 86400);
        await m.withField('spec.template.spec.restartPolicy', 'Never');
        await m.withField('spec.template.spec.automountServiceAccountToken', false);
      },
    });
    // Database backups: plugin-barman-cloud (installed in the cluster, cnpg-system) writes base backups and WAL to
    // Azure Blob (account in values backup.azure.account, path in backup.destinationPath). The platform Terraform puts
    // the account key into the secret store, the ExternalSecret below brings it in (spec.data in deploy/overlay).
    // https://cloudnative-pg.io/plugin-barman-cloud/docs/usage/
    await resource.addManifest('external-secrets.io/v1', 'ExternalSecret', 'hub-db-backup-storage', {
      configure: async (m) => {
        // Without the secret no WAL is written: it must not be deleted together with the ExternalSecret.
        await m.withAnnotation('argocd.argoproj.io/sync-options', 'Prune=false,Delete=false');
        await m.withAnnotation('argocd.argoproj.io/sync-wave', '-3');
        await m.withField('spec.refreshInterval', '1h');
        await m.withField('spec.secretStoreRef.kind', '{{ .Values.secrets.store.kind }}');
        await m.withField('spec.secretStoreRef.name', '{{ .Values.secrets.store.name }}');
        await m.withField('spec.target.name', 'hub-db-backup-storage');
        await m.withField('spec.target.creationPolicy', 'Owner');
        await m.withField('spec.target.deletionPolicy', 'Retain');
        await m.withField('spec.target.template.mergePolicy', 'Merge');
        await m.withField('spec.target.template.data.AZURE_STORAGE_ACCOUNT', '{{ .Values.backup.azure.account }}');
      },
    });
    // barman enforces the retention of backups and WAL (values backup.retention). The plugin sidecar resources in the
    // database pod are set here: without limits.cpu a namespace quota rejects the pod (plugin v0.15.1 takes them all from here).
    await resource.addManifest('barmancloud.cnpg.io/v1', 'ObjectStore', 'hub-db-backups', {
      configure: async (m) => {
        // Without the ObjectStore WAL archiving of the live database stops: Argo CD prune must not touch it.
        await m.withAnnotation('argocd.argoproj.io/sync-options', 'Prune=false,Delete=false');
        await m.withAnnotation('argocd.argoproj.io/sync-wave', '-3');
        await m.withField('spec.retentionPolicy', '{{ .Values.backup.retention }}');
        // Keep the path byte for byte: a different path is a new archive for barman, and the WAL chain breaks.
        await m.withField('spec.configuration.destinationPath', '{{ .Values.backup.destinationPath }}');
        await m.withField('spec.configuration.azureCredentials.storageAccount.name', 'hub-db-backup-storage');
        await m.withField('spec.configuration.azureCredentials.storageAccount.key', 'AZURE_STORAGE_ACCOUNT');
        await m.withField('spec.configuration.azureCredentials.storageKey.name', 'hub-db-backup-storage');
        await m.withField('spec.configuration.azureCredentials.storageKey.key', 'AZURE_STORAGE_KEY');
        await m.withField('spec.configuration.wal.compression', 'gzip');
        await m.withField('spec.configuration.data.compression', 'gzip');
        await m.withField('spec.instanceSidecarConfiguration.resources.requests.cpu', '20m');
        await m.withField('spec.instanceSidecarConfiguration.resources.requests.memory', '64Mi');
        await m.withField('spec.instanceSidecarConfiguration.resources.limits.cpu', '300m');
        await m.withField('spec.instanceSidecarConfiguration.resources.limits.memory', '256Mi');
      },
    });
    // A base backup on schedule (values backup.schedule), the first one right on creation (immediate). WAL streams
    // continuously. Wave 1: Argo creates it after the Cluster with the plugin (deploy/overlay) is healthy again; created
    // earlier, the immediate backup can start before the plugin is ready and fail.
    await resource.addManifest('postgresql.cnpg.io/v1', 'ScheduledBackup', 'hub-db-daily', {
      configure: async (m) => {
        await m.withAnnotation('argocd.argoproj.io/sync-wave', '1');
        await m.withField('spec.schedule', '{{ .Values.backup.schedule }}');
        await m.withField('spec.immediate', true);
        await m.withField('spec.backupOwnerReference', 'self');
        await m.withField('spec.cluster.name', 'hub-db');
        await m.withField('spec.method', 'plugin');
        await m.withField('spec.pluginConfiguration.name', 'barman-cloud.cloudnative-pg.io');
      },
    });
    // Caller secret of hub-web: hub requires it on /api, hub-web sends it in Authorization.
    // The platform Terraform writes the secret store key (random_password, one key per caller);
    // spec.data in deploy/overlay. Both pods read the secret as a file from a volume.
    // Wave -1: the secret exists before the wave-0 Deployments (without a wave, ExternalSecret as its own kind would
    // sync after them). The next caller gets the same kind of ExternalSecret with its own key and one more source in
    // the callers volume of hub.
    await resource.addManifest('external-secrets.io/v1', 'ExternalSecret', 'hub-caller-hub-web', {
      configure: async (m) => {
        await m.withAnnotation('argocd.argoproj.io/sync-wave', '-1');
        await m.withField('spec.refreshInterval', '5m');
        await m.withField('spec.secretStoreRef.kind', '{{ .Values.secrets.store.kind }}');
        await m.withField('spec.secretStoreRef.name', '{{ .Values.secrets.store.name }}');
        await m.withField('spec.target.name', 'hub-caller-hub-web');
        await m.withField('spec.target.creationPolicy', 'Owner');
      },
    });
    // Database roles hub and hub_web: each service has its own role with rights on its own schemas only
    // (services/hub/migrations/0021_db_roles.sql), the owner app stays the migration role. The password comes from a
    // secret store key per service (random_password, written by the platform Terraform); ESO builds a basic-auth secret
    // with username, password and uri from it. CNPG reads it (spec.managed.roles of the cluster hub-db), and so does the
    // service (HUBDB_URI). Wave -3: the secret exists before the cluster (-2) and the pods. Template and data in
    // deploy/overlay (arrays, keys with dots).
    for (const svc of ['hub', 'hub-web']) {
      await resource.addManifest('external-secrets.io/v1', 'ExternalSecret', `hub-db-${svc}`, {
        configure: async (m) => {
          await m.withAnnotation('argocd.argoproj.io/sync-wave', '-3');
          await m.withField('spec.refreshInterval', '1h');
          await m.withField('spec.secretStoreRef.kind', '{{ .Values.secrets.store.kind }}');
          await m.withField('spec.secretStoreRef.name', '{{ .Values.secrets.store.name }}');
          await m.withField('spec.target.name', `hub-db-${svc}`);
          await m.withField('spec.target.creationPolicy', 'Owner');
        },
      });
    }
    // Other callers (for example the document run docs-<repo> of each repository) come from values hub.callers:
    // deploy/overlay/templates/hub/caller-extra.tpl, the AppHost cannot loop over values.
  });
} else {
  // Locally Postgres runs in a container: https://aspire.dev/integrations/databases/postgres/postgres-host/
  // withReference puts the variable HUBDB_URI into the hub environment.
  const pg = await builder.addPostgres('pg');
  const hubdb = await pg.addDatabase('hubdb');
  // Migrations run as a separate process, like the Job hub-db-migrate in the cluster: hub does not run them itself and
  // waits until hub-migrate completes successfully.
  const migrate = await builder.addRustApp('hub-migrate', '../services');
  await migrate.withCargoPackage('hub');
  await migrate.withArgs(['migrate']);
  await migrate.withOtlpExporter({ protocol: OtlpProtocol.HttpProtobuf });
  await migrate.withCertificateTrustScope(CertificateTrustScope.System);
  await migrate.withReference(hubdb);
  await migrate.waitFor(hubdb);
  // Install settings: required by both hub migrate and hub; locally a demo account. In the cluster the same env block
  // is in deploy/overlay (Job and Deployment hub), the values come from the install.
  for (const app of [migrate, hub]) {
    await app.withEnvironment('HUB_ACCOUNT', 'demo');
    await app.withEnvironment('HUB_TIMEZONE', 'UTC');
  }
  await hub.withReference(hubdb);
  await hub.waitForCompletion(migrate);
  // The example seeds (examples/seed: people, components, the repository registry, one plan scope). In the cluster an
  // install sets HUB_SEED_DIR and the like in its values (hub.env); they do not come from here.
  await hub.withEnvironment('HUB_SEED_DIR', fileURLToPath(new URL('../examples/seed', import.meta.url)));
  await hub.withEnvironment('HUB_SEED_REQUIRED', 'true');
  localDb = hubdb;
}

// https://aspire.dev/integrations/frameworks/javascript/ and https://aspire.dev/deployment/javascript-apps/
// withReference(hub) puts the variable HUB_HTTP into the hub-web environment.
const web = await builder.addViteApp('hub-web', '../hub-web');
await web.withPnpm();
await web.withOtlpExporter({ protocol: OtlpProtocol.HttpProtobuf });
await web.withReference(hub);
await web.waitFor(hub);
await web.withExternalHttpEndpoints(); // without it addIngress refuses to reference the endpoint
await web.publishAsNodeServer('build/index.js', { outputPath: 'build' });
// /health of hub-web does not call hub. Locally no health check is needed: nothing waits for hub-web.
if (publish) await web.withHttpProbe(ProbeType.Readiness, { path: '/health' });

// Better Auth (sign-in for MCP clients): HUBDB_URI for the auth schema tables, BETTER_AUTH_SECRET, ORIGIN.
if (publish) {
  // The secret lives in the secret store (values secrets.remoteKeys.webAuthSecret): losing the namespace does not sign
  // everyone out. deploy/overlay wires HUBDB_URI and BETTER_AUTH_SECRET (secretKeyRef) and holds the ExternalSecret
  // spec.data: withField cannot express arrays.
  await web.publishAsKubernetesService(async (resource) => {
    await resource.addManifest('external-secrets.io/v1', 'ExternalSecret', 'hub-web-auth', {
      configure: async (m) => {
        // Deleting the ExternalSecret would delete the secret too, and with it all sessions and MCP tokens.
        await m.withAnnotation('argocd.argoproj.io/sync-options', 'Prune=false,Delete=false');
        await m.withField('spec.refreshInterval', '1h');
        await m.withField('spec.secretStoreRef.kind', '{{ .Values.secrets.store.kind }}');
        await m.withField('spec.secretStoreRef.name', '{{ .Values.secrets.store.name }}');
        await m.withField('spec.target.name', 'hub-web-auth');
        await m.withField('spec.target.creationPolicy', 'Owner');
        await m.withField('spec.target.deletionPolicy', 'Retain');
      },
    });
    // What hub-web needs to verify the Access JWT (Cf-Access-Jwt-Assertion): the application audience and team domain.
    // The platform Terraform writes them; data (secret store keys) in deploy/overlay.
    await resource.addManifest('external-secrets.io/v1', 'ExternalSecret', 'hub-web-access', {
      configure: async (m) => {
        await m.withField('spec.refreshInterval', '1h');
        await m.withField('spec.secretStoreRef.kind', '{{ .Values.secrets.store.kind }}');
        await m.withField('spec.secretStoreRef.name', '{{ .Values.secrets.store.name }}');
        await m.withField('spec.target.name', 'hub-web-access');
      },
    });
    // Path tokens of the agent mailbox (MAILBOX_PATH_TOKENS, lib/server/mailbox-gate.ts): the platform Terraform writes
    // them; data (secret store key) and the condition in deploy/overlay. No key in values: the mailbox stays closed.
    await resource.addManifest('external-secrets.io/v1', 'ExternalSecret', 'hub-web-mailbox', {
      configure: async (m) => {
        await m.withField('spec.refreshInterval', '1h');
        await m.withField('spec.secretStoreRef.kind', '{{ .Values.secrets.store.kind }}');
        await m.withField('spec.secretStoreRef.name', '{{ .Values.secrets.store.name }}');
        await m.withField('spec.target.name', 'hub-web-mailbox');
      },
    });
    // Client secret of the OIDC entrance (AUTH_OIDC_CLIENT_SECRET_FILE, lib/server/identity/oidc.ts): data (secret store
    // key) and the condition in deploy/overlay. No key in values: no ExternalSecret.
    await resource.addManifest('external-secrets.io/v1', 'ExternalSecret', 'hub-web-oidc', {
      configure: async (m) => {
        await m.withField('spec.refreshInterval', '1h');
        await m.withField('spec.secretStoreRef.kind', '{{ .Values.secrets.store.kind }}');
        await m.withField('spec.secretStoreRef.name', '{{ .Values.secrets.store.name }}');
        await m.withField('spec.target.name', 'hub-web-oidc');
      },
    });
  });
} else {
  const secret = await builder.addParameterWithGeneratedValue(
    'better-auth-secret',
    { minLength: 48, special: false },
    { secret: true, persist: true },
  );
  await web.withEnvironment('BETTER_AUTH_SECRET', secret);
  await web.withReference(localDb!);
  await web.waitFor(localDb!);
  await web.withEnvironment('ORIGIN', await web.getEndpoint('http'));
  // Without Tailscale in front of the local hub-web, sign-in uses this login.
  await web.withEnvironment('AUTH_DEV_LOGIN', 'dev@local');
}

if (publish) {
  // Two entrances, both optional. The main address (values origin) goes through Cloudflare (Access for people, a tunnel
  // managed by the platform Terraform) and the cloudflared connector below. It is also the adapter-node ORIGIN, the
  // Better Auth baseURL and the MCP resource address (<origin>/mcp). The tailnet entrance: forms from it are accepted
  // (AUTH_EXTRA_ORIGINS), and hub-web trusts Tailscale headers only for requests to AUTH_TAILSCALE_HOSTS.
  // ORIGIN, AUTH_TAILSCALE_HOSTS and AUTH_EXTRA_ORIGINS come from values (origin, auth.*): deploy/overlay/templates/hub-web/config.yaml.

  // Tunnel connector: an outbound connection to Cloudflare; the platform Terraform manages the tunnel ingress rules.
  // It also writes the run token (TUNNEL_TOKEN) to the secret store; ExternalSecret below, secretKeyRef and data in deploy/overlay.
  // https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/deployment-guides/kubernetes/
  const tunnel = await builder.addContainer('cloudflared', {
    image: 'cloudflare/cloudflared',
    tag: '2026.5.2@sha256:12ff5c6992a9863db4da270746af7c244bcaee49353039af8104268a18d6c4f0',
  });
  await tunnel.withImageRegistry('docker.io');
  // Protocol from values (default http2): where QUIC (UDP 7844) does not get through, the connector tries QUIC for
  // minutes before it falls back to http2.
  await tunnel.withArgs([
    'tunnel', '--no-autoupdate', '--protocol', '{{ .Values.ingress.cloudflared.protocol }}', '--metrics', '0.0.0.0:2000', 'run',
  ]);
  await tunnel.withHttpEndpoint({ name: 'metrics', targetPort: 2000 });
  // /ready answers 200 when there is at least one connection to Cloudflare.
  await tunnel.withHttpProbe(ProbeType.Readiness, { path: '/ready', endpointName: 'metrics' });
  await tunnel.publishAsKubernetesService(async (resource) => {
    await resource.addManifest('external-secrets.io/v1', 'ExternalSecret', 'cloudflared-tunnel', {
      configure: async (m) => {
        await m.withField('spec.refreshInterval', '1h');
        await m.withField('spec.secretStoreRef.kind', '{{ .Values.secrets.store.kind }}');
        await m.withField('spec.secretStoreRef.name', '{{ .Values.secrets.store.name }}');
        await m.withField('spec.target.name', 'cloudflared-tunnel');
      },
    });
  });

  // https://aspire.dev/deployment/kubernetes-ingress/
  const ingress = await k8s.addIngress('hub-web-ingress');
  // Class and proxy-group come from values in deploy/overlay: Aspire strips {{ }} in Ingress annotations and class.
  await ingress.withDefaultBackend(await web.getEndpoint('http'));
  // withIngressTlsAuto() was tried: it yields tls with secretName "hub-web-ingress-tls" and no hosts,
  // while Tailscale takes the name from tls.hosts. So tls.hosts comes from the overlay.
}

await builder.build().run();
