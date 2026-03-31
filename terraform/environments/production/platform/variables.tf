variable "project_id" {
  description = "GCP project ID for production."
  type        = string
}

variable "region" {
  description = "Primary production region."
  type        = string
}

variable "environment" {
  description = "Environment name."
  type        = string
  default     = "production"
}

variable "cluster_name" {
  description = "GKE cluster name created by the foundation stack."
  type        = string
  default     = "dragon-shift-prod"
}

variable "cluster_location" {
  description = "Optional GKE cluster location override. Defaults to region when empty."
  type        = string
  default     = ""
}

variable "namespace" {
  description = "Kubernetes namespace for the app."
  type        = string
  default     = "dragon-shift"
}

variable "hostname" {
  description = "Public production hostname."
  type        = string
}

variable "dns_zone_name" {
  description = "Cloud DNS managed zone name."
  type        = string
}

variable "dns_zone_dns_name" {
  description = "Cloud DNS zone DNS suffix, e.g. example.com."
  type        = string
}

variable "image_repository" {
  description = "Container image repository for the Helm release."
  type        = string
}

variable "image_digest" {
  description = "Immutable container image digest. Preferred for production."
  type        = string
  default     = ""
}

variable "image_tag" {
  description = "Mutable container image tag. Used only when digest is empty."
  type        = string
  default     = "main"
}

variable "helm_chart_path" {
  description = "Absolute or relative path to the Helm chart."
  type        = string
  default     = "../../../../helm/dragon-shift"
}

variable "release_name" {
  description = "Helm release name."
  type        = string
  default     = "dragon-shift"
}

variable "trust_forwarded_for" {
  description = "Whether the app should trust X-Forwarded-For from the GCLB edge."
  type        = bool
  default     = true
}

variable "database_pool_size" {
  description = "Postgres connection pool size."
  type        = number
  default     = 10
}

variable "rust_session_code_prefix" {
  description = "Single-digit session code prefix."
  type        = string
  default     = "9"
}

variable "create_rate_limit_max" {
  description = "Per-minute create workshop limit."
  type        = number
  default     = 20
}

variable "join_rate_limit_max" {
  description = "Per-minute join/reconnect limit."
  type        = number
  default     = 60
}

variable "command_rate_limit_max" {
  description = "Per-minute command rate limit."
  type        = number
  default     = 180
}

variable "websocket_rate_limit_max" {
  description = "Per-minute websocket rate limit."
  type        = number
  default     = 500
}

variable "cloud_armor_rate_limit_count" {
  description = "Cloud Armor rate limit count per interval."
  type        = number
  default     = 600
}

variable "cloud_armor_rate_limit_interval_sec" {
  description = "Cloud Armor rate limit interval seconds."
  type        = number
  default     = 60
}

variable "notification_channel_id" {
  description = "Optional Monitoring notification channel ID. Required when enable_uptime_checks=true."
  type        = string
  default     = ""
}

variable "database_url_secret_id" {
  description = "Secret Manager secret ID that stores the runtime DATABASE_URL."
  type        = string
  default     = "dragon-shift-production-database-url"
}

variable "enable_uptime_checks" {
  description = "Whether to create uptime checks and the related alert policy. Keep false until DNS delegation and managed certificate issuance are complete."
  type        = bool
  default     = false
}

variable "labels" {
  description = "Common resource labels."
  type        = map(string)
  default     = {}
}

variable "kubeconfig_path" {
  description = "Optional kubeconfig path for platform apply operations, for example a Connect Gateway kubeconfig. When set, the kubernetes and helm providers use this kubeconfig instead of the foundation cluster endpoint outputs."
  type        = string
  default     = ""
}

variable "kubeconfig_context" {
  description = "Optional kubeconfig context name to use with kubeconfig_path."
  type        = string
  default     = ""
}
