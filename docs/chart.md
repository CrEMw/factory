# Helm chart generation

The Helm chart in `deploy/chart` is generated, never edited by hand. Its sources:

- `apphost/apphost.mts`: the Aspire TypeScript AppHost, the service graph (hub, hub-web, Postgres, the cloudflared
  connector, the Ingress) and extra manifests added with `addManifest`;
- `deploy/overlay`: patches for what the TypeScript AppHost cannot express, extra plain Helm templates and the
  install values with their neutral defaults;
- `tools/chart.sh`: the script that runs `aspire publish` and applies the overlay.

The generated chart is committed. `tools/check.sh` regenerates it and fails on any difference.

## Why an overlay

The TypeScript AppHost SDK (version in `aspire.config.json`) cannot express some fields: resource limits,
`nodeSelector`, `secretKeyRef` env entries, rollout strategy, `tls.hosts` of the Ingress, arrays and keys with dots in
`addManifest` resources. `deploy/overlay` adds them after `aspire publish`. Values are read from `.Values` in both
places; their defaults live in `deploy/overlay/values.yaml` (see [configuration.md](configuration.md)).

## tools/chart.sh

```sh
tools/chart.sh
```

Steps:

1. Remove `deploy/chart` and run `aspire publish -o deploy/chart`.
2. For each `deploy/overlay/templates/**/*.yaml` (a patch), merge it into the generated template with the same path,
   with yq deep merge (`*d`).
3. Merge `deploy/overlay/values.yaml` into the generated `values.yaml`: maps merge deeply, lists and scalars of the
   overlay replace the generated ones. Keys Aspire did not generate are added; this is how values of overlay
   templates reach the chart.
4. Copy `deploy/overlay/templates/**/*.tpl` into the chart as is.
5. Turn markers into Helm text (below) and replace the Aspire `{{ .Chart.Name }}` labels.

Environment:

| Variable | Default | Meaning |
|---|---|---|
| `ASPIRE` | `.tools/aspire` | Aspire CLI. |
| `YQ` | `mise exec yq@4.54.3 -- yq` | yq command. |
| `CHECK_SH_ROOT` | the repository the script lives in | Root of the repository whose chart to generate. |

### Safety checks

The script stops with an error when a patch could land on the wrong place:

- a patch has no generated template at the same path (the AppHost renamed the resource);
- a patch has no `kind`, or its `kind` differs from the generated one (`kind` is checked, not merged);
- a patch brings `spec.tls` where Aspire already generated one;
- a container in a patch has no `name`;
- a patch brings `env` for a container where Aspire already generated an env list;
- the set or order of container names changes after the merge (unless the generated template had no containers);
- a `{{=` marker is left unresolved.

Arrays merge by index, which is why these checks exist.

## Patch format

A patch is a partial manifest. Its first line declares the `kind` it was written for:

```yaml
kind: Deployment
spec:
  template:
    spec:
      containers:
        - name: hub
          resources:
            limits:
              memory: 256Mi
```

## Markers

Helm text that YAML cannot hold is written as a marker string and turned into Helm text after all yq steps.

| Marker | Result | Use |
|---|---|---|
| `'{{= X }}'` | `{{ X }}` without quotes | An unquoted value: a number, `toYaml`, `include`. Single or double quotes. |
| `- '{{=- X }}'` | the line `{{- X }}` replacing the whole list item | `if`, `end`, a block `include` inside a list. |
| `x-helm-if: X` | the template wrapped in `{{- if X }}` ... `{{- end }}` | A top-level key of a patch: render the whole template only when `X` holds. Not merged into the chart. |

Examples, abbreviated from the overlay:

```yaml
kind: Deployment
x-helm-if: .Values.ingress.cloudflared.enabled
spec:
  template:
    spec:
      nodeSelector: '{{=- toYaml .Values.nodeSelector | nindent 8 }}'
```

```yaml
env:
  - '{{=- if .Values.auth.providers.cloudflareAccess.enabled }}'
  - name: AUTH_CF_ACCESS_AUD
    valueFrom:
      secretKeyRef:
        name: hub-web-access
        key: aud
  - '{{=- end }}'
```

Plain Helm strings such as `"{{ .Values.origin }}"` need no marker: they stay quoted strings.

## Chart name

The AppHost sets the chart name `factory`. When `deploy/overlay/values.yaml` has the key `nameOverride`, the script
replaces `{{ .Chart.Name }}` in the generated templates with `{{ .Values.nameOverride | default .Chart.Name }}`. The
name is part of the immutable Deployment selectors (`app.kubernetes.io/name`), so an install already running under
another name sets `nameOverride`.

## Plain templates

`deploy/overlay/templates/**/*.tpl` are copied unchanged:

- `_helpers.tpl`: the shared env blocks (`hub.instance-env`, `hub.extra-env`) and the caller volume sources
  (`hub.caller-sources`);
- `hub/caller-extra.tpl`: one ExternalSecret `hub-caller-<name>` per entry of `hub.callers`, since the AppHost cannot
  loop over values.

## Drift check

The `chart` part of `tools/check.sh` restores the AppHost dependencies (`npm ci`, `aspire restore`), copies
`deploy/chart`, runs `tools/chart.sh` and compares the result with the copy. Any difference fails:

```
deploy/chart drifts from AppHost and overlay: run tools/chart.sh and commit the result
```

```sh
tools/check.sh chart   # only this part
tools/check.sh         # the parts the branch touches
tools/check.sh --all   # every part
```

Without arguments the script picks parts from the diff against `origin/main`; a change under `apphost/`, `deploy/` or
`tools/`, or to `aspire.config.json` or a `Dockerfile`, selects `apphost`, `chart` and `helm-lint`. `helm-lint` runs
`helm lint` on the chart with probe values for the required keys.

`tools/check.sh` runs `tools/chart.sh` from its own directory, not from `CHECK_SH_ROOT` (the tree under check). A CI
gate that takes `tools/check.sh` and `tools/chart.sh` from the main branch therefore checks a change with the merged
script, so a change cannot weaken its own check; a change to `tools/chart.sh` itself takes effect there after it is merged.

## Changing the chart

1. Change `apphost/apphost.mts`, a file in `deploy/overlay`, or both.
2. Run `tools/chart.sh`.
3. Commit the source change together with the regenerated `deploy/chart`.
