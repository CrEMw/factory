{{- /*
One ExternalSecret hub-caller-<name> per entry of hub.callers (values): the callers of hub besides hub-web, for example
the document run docs-<repo> of a repository, which sends its snapshot to POST /api/documents/sync with this secret.
Secret store key from values secrets.remoteKeys.callers.<name> (random_password, written by the platform Terraform).
hub reads the secrets from the volume callers (deploy/overlay/templates/hub/deployment.yaml, _helpers.tpl). Plain Helm:
tools/chart.sh copies .tpl files as is. The same fields as the AppHost resources: wave -1, labels of the chart.
*/ -}}
{{- range sortAlpha .Values.hub.callers }}
---
apiVersion: external-secrets.io/v1
kind: ExternalSecret
metadata:
  name: "hub-caller-{{ . }}"
  annotations:
    argocd.argoproj.io/sync-wave: "-1"
  labels:
    app.kubernetes.io/name: "{{ $.Values.nameOverride | default $.Chart.Name }}"
    app.kubernetes.io/component: "hub"
    app.kubernetes.io/instance: "{{ $.Release.Name }}"
# Secret of the caller {{ . }} (values hub.callers): the caller sends it to hub. Secret store key from values
# secrets.remoteKeys.callers (random_password, written by the platform Terraform). hub reads it (volume callers,
# deploy/overlay/templates/hub/deployment.yaml).
spec:
  refreshInterval: "5m"
  secretStoreRef:
    kind: "{{ $.Values.secrets.store.kind }}"
    name: "{{ $.Values.secrets.store.name }}"
  target:
    name: "hub-caller-{{ . }}"
    creationPolicy: "Owner"
  data:
    - secretKey: secret
      remoteRef:
        key: {{ index $.Values.secrets.remoteKeys.callers . | required (printf "secrets.remoteKeys.callers.%s" .) }}
{{- end }}
