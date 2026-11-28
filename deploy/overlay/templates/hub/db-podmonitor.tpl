{{- /*
PodMonitor of the database hub-db (values db.podMonitor): the Prometheus Operator scrapes the metrics port of every
CNPG instance pod. Off by default: the PodMonitor CRD exists only where the Prometheus Operator is installed.
db.podMonitor.labels go on the PodMonitor so that the podMonitorSelector of Prometheus picks it (for example
release: <name of the kube-prometheus-stack release>). The PodMonitor of CNPG itself (spec.monitoring.enablePodMonitor)
is deprecated: https://cloudnative-pg.io/documentation/current/monitoring/
Plain Helm: the AppHost cannot express the keys with dots in the selector and the endpoint array, tools/chart.sh copies
.tpl files as is. The same labels as the AppHost resources.
*/ -}}
{{- if .Values.db.podMonitor.enabled }}
---
apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: "hub-db"
  labels:
    app.kubernetes.io/name: "{{ .Values.nameOverride | default .Chart.Name }}"
    app.kubernetes.io/component: "hub"
    app.kubernetes.io/instance: "{{ .Release.Name }}"
    {{- with .Values.db.podMonitor.labels }}
    {{- toYaml . | nindent 4 }}
    {{- end }}
spec:
  selector:
    matchLabels:
      cnpg.io/cluster: "hub-db"
      cnpg.io/podRole: "instance"
  podMetricsEndpoints:
    - port: "metrics"
{{- end }}
