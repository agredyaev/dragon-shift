{{- define "dragon-shift.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "dragon-shift.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name (include "dragon-shift.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "dragon-shift.labels" -}}
app.kubernetes.io/name: {{ include "dragon-shift.name" . }}
helm.sh/chart: {{ .Chart.Name }}-{{ .Chart.Version | replace "+" "_" }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "dragon-shift.selectorLabels" -}}
app.kubernetes.io/name: {{ include "dragon-shift.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "dragon-shift.app.fullname" -}}
{{- include "dragon-shift.fullname" . -}}
{{- end -}}

{{- define "dragon-shift.app.labels" -}}
{{ include "dragon-shift.labels" . }}
app.kubernetes.io/component: app-server
{{- end -}}

{{- define "dragon-shift.app.selectorLabels" -}}
{{ include "dragon-shift.selectorLabels" . }}
app.kubernetes.io/component: app-server
{{- end -}}

{{- define "dragon-shift.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "dragon-shift.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "dragon-shift.postgresql.fullname" -}}
{{- printf "%s-postgresql" (include "dragon-shift.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "dragon-shift.postgresql.secretName" -}}
{{- printf "%s-postgresql" (include "dragon-shift.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "dragon-shift.databaseUrl" -}}
{{- printf "postgres://%s:%s@%s:%v/%s" (.Values.postgresql.auth.username | urlquery) (.Values.postgresql.auth.password | urlquery) (include "dragon-shift.postgresql.fullname" .) (.Values.postgresql.service.port | int) .Values.postgresql.auth.database -}}
{{- end -}}

{{- define "dragon-shift.gateway.fullname" -}}
{{- printf "%s-gateway" (include "dragon-shift.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "dragon-shift.coordinator.fullname" -}}
{{- printf "%s-coordinator" (include "dragon-shift.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "dragon-shift.gateway.labels" -}}
{{ include "dragon-shift.labels" . }}
app.kubernetes.io/component: gateway
{{- end -}}

{{- define "dragon-shift.coordinator.labels" -}}
{{ include "dragon-shift.labels" . }}
app.kubernetes.io/component: coordinator
{{- end -}}

{{- define "dragon-shift.rustCoordinator.fullname" -}}
{{- printf "%s-rust-coordinator" (include "dragon-shift.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "dragon-shift.rustCoordinator.labels" -}}
{{ include "dragon-shift.labels" . }}
app.kubernetes.io/component: rust-coordinator
{{- end -}}
