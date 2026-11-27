{{/* Install settings: one block for the Job hub-db-migrate and the Deployment hub. */}}
{{- define "hub.instance-env" -}}
{{- $i := .Values.hub.instance -}}
- name: HUB_ACCOUNT
  value: {{ required "hub.instance.account" $i.account | toString | toYaml }}
- name: HUB_TIMEZONE
  value: {{ required "hub.instance.timezone" $i.timezone | toString | toYaml }}
{{- $m := .Values.hub.migrations }}
{{- if not (has $m.source (list "baseline" "chain")) }}
{{- fail (printf "hub.migrations.source: %v, expected baseline or chain" $m.source) }}
{{- end }}
{{- if and $m.adoptChecksums (ne $m.source "baseline") }}
{{- fail "hub.migrations.adoptChecksums needs hub.migrations.source: baseline" }}
{{- end }}
- name: HUB_MIGRATIONS
  value: {{ $m.source | quote }}
{{- with $i.mailboxPendingLimit }}
- name: HUB_MAILBOX_PENDING_LIMIT
  value: {{ toString . | quote }}
{{- end }}
{{- with $i.mailboxPendingTtl }}
- name: HUB_MAILBOX_PENDING_TTL
  value: {{ toString . | quote }}
{{- end }}
{{- with $i.documentRoots }}
- name: HUB_DOCUMENT_ROOTS
  value: {{ join "," . | quote }}
{{- end }}
{{- with $i.documentLinkLabels }}
- name: HUB_DOCUMENT_LINK_LABELS
  value: {{ toJson . | quote }}
{{- end }}
{{- end }}

{{/* otel.endpoint and an env map (dict "root" . "env" <map>) as an env list; empty when there is nothing. */}}
{{- define "hub.extra-env" -}}
{{- $out := list -}}
{{- with .root.Values.otel.endpoint }}
{{- $out = append $out (dict "name" "OTEL_EXPORTER_OTLP_ENDPOINT" "value" (toString .)) }}
{{- end }}
{{- range $k, $v := .env }}
{{- if regexMatch "(SECRET|PASSWORD|TOKEN|PRIVATE_KEY)" (upper $k) }}
{{- fail (printf "env %s: a secret does not go into values env, where it lands in the Deployment spec as plain text; use secrets.remoteKeys (docs/configuration.md)" $k) }}
{{- end }}
{{- if and (has $k (list "HUB_SEED_DIR" "HUB_SEED_REQUIRED")) $.root.Values.hub.seed.files }}
{{- fail (printf "env %s: set by hub.seed.files, not in hub.env" $k) }}
{{- end }}
{{- $out = append $out (dict "name" $k "value" (toString $v)) }}
{{- end }}
{{- if $out }}{{ toYaml $out }}{{ end }}
{{- end }}

{{/* The callers of hub.callers as sources of the projected volume callers of hub (templates/hub/deployment.yaml),
in list order: one secret hub-caller-<name> with the file <name> each (src/callers.rs). */}}
{{- define "hub.caller-sources" -}}
{{- range .Values.hub.callers }}
{{- if hasPrefix "docs-" . }}
# Document run of the repository {{ trimPrefix "docs-" . }}: only POST /api/documents/sync for its own repository.
{{- end }}
- secret:
    name: hub-caller-{{ . }}
    items:
      - key: secret
        path: {{ . }}
{{- end }}
{{- end }}

{{/* hub.seed.files (path: content) as ConfigMap data: a ConfigMap key cannot hold "/", so links/a.jsonl is the key
links__a.jsonl and the volume maps it back (hub.seed-items). A path is a file name or one directory and a file name. */}}
{{- define "hub.seed-data" -}}
{{- range $path, $body := .Values.hub.seed.files }}
{{- if or (not (regexMatch "^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)?$" $path)) (contains "__" $path) (hasPrefix "." $path) }}
{{- fail (printf "hub.seed.files: %q is not a seed path (a file name or dir/file, letters, digits, . _ -, no __)" $path) }}
{{- end }}
{{ replace "/" "__" $path | quote }}: {{ toString $body | quote }}
{{- end }}
{{- end }}

{{/* The items of the volume seed of hub: each ConfigMap key back at its path under HUB_SEED_DIR. */}}
{{- define "hub.seed-items" -}}
{{- range $path, $_ := .Values.hub.seed.files }}
- key: {{ replace "/" "__" $path | quote }}
  path: {{ $path | quote }}
{{- end }}
{{- end }}

{{/* Pod annotations of hub: the hash of hub.seed.files when set, else none. */}}
{{- define "hub.seed-annotations" -}}
{{- if .Values.hub.seed.files }}
checksum/hub-seed: {{ .Values.hub.seed.files | toJson | sha256sum | quote }}
{{- else -}}
{}
{{- end }}
{{- end }}

{{/* CNPG plugins of Cluster hub-db: plugin-barman-cloud writes backups and WAL to the ObjectStore hub-db-backups
when backup.enabled; else an empty list, not null (a null plugins fails schema validation of the Cluster). */}}
{{- define "hub.db-plugins" -}}
{{- if .Values.backup.enabled }}
- name: barman-cloud.cloudnative-pg.io
  isWALArchiver: true
  parameters:
    barmanObjectName: hub-db-backups
{{- else -}}
[]
{{- end }}
{{- end }}
