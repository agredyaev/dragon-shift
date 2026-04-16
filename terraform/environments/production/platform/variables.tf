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
  description = "Public production hostname. Required when hostname_mode is managed_dns or external_dns."
  type        = string
  default     = ""
}

variable "hostname_mode" {
  description = "Hostname strategy for production: managed_dns creates a Cloud DNS zone/record, external_dns expects an externally managed hostname, and nip_io derives a public hostname from the reserved global IP."
  type        = string
  default     = "nip_io"
}

variable "dns_zone_name" {
  description = "Cloud DNS managed zone name. Required only when hostname_mode=managed_dns."
  type        = string
  default     = ""
}

variable "dns_zone_dns_name" {
  description = "Cloud DNS zone DNS suffix, e.g. example.com. Required only when hostname_mode=managed_dns."
  type        = string
  default     = ""
}

variable "nip_io_label" {
  description = "Leftmost DNS label to use when hostname_mode=nip_io."
  type        = string
  default     = "dragon-shift"
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

variable "app_cpu_request" {
  description = "Optional CPU request override for the production app pod. When null, the Helm chart default is used."
  type        = string
  default     = null
}

variable "app_cpu_limit" {
  description = "Optional CPU limit override for the production app pod. When null, the Helm chart default is used."
  type        = string
  default     = null
}

variable "app_memory_request" {
  description = "Optional memory request override for the production app pod. When null, the Helm chart default is used."
  type        = string
  default     = null
}

variable "app_memory_limit" {
  description = "Optional memory limit override for the production app pod. When null, the Helm chart default is used."
  type        = string
  default     = null
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

variable "enable_cloud_armor" {
  description = "Whether to create and attach the Cloud Armor security policy. Disable when the project has no Cloud Armor quota."
  type        = bool
  default     = true
}

variable "notification_channel_id" {
  description = "Optional Monitoring notification channel ID override. When empty, the automated production apply uses the foundation stack output."
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

# ---------------------------------------------------------------------------
# Gemini / Vertex AI
# ---------------------------------------------------------------------------

variable "gemini_api_key" {
  description = "Gemini API key for LLM judge and image generation. When set, a K8s secret dragon-shift-llm is created and api_key providers are configured in the Helm release. Leave empty to rely solely on Vertex AI with Workload Identity."
  type        = string
  default     = ""
  sensitive   = true
}

variable "gemini_api_keys" {
  description = "Optional additional Gemini API keys for api_key mode. Production automation can populate this list from multiple GitHub secrets to spread requests across several provider entries."
  type        = list(string)
  default     = []
  sensitive   = true
}

variable "google_cloud_project" {
  description = "Google Cloud project ID passed to the app runtime for Vertex AI calls. Required for vertex_ai providers and defaults to var.project_id in production automation."
  type        = string
  default     = ""
}

variable "google_cloud_location" {
  description = "Google Cloud region for Vertex AI endpoint routing. Required for vertex_ai providers and defaults to var.region in production automation."
  type        = string
  default     = ""
}

variable "llm_judge_model" {
  description = "Model name for the LLM judge provider."
  type        = string
  default     = "gemini-2.5-flash"
}

variable "llm_image_model" {
  description = "Model name for the LLM image generation provider."
  type        = string
  default     = "gemini-2.5-flash-image"
}

variable "llm_provider_type" {
  description = "LLM provider type: vertex_ai (Workload Identity, no API key) or api_key (requires gemini_api_key)."
  type        = string
  default     = "vertex_ai"

  validation {
    condition     = contains(["vertex_ai", "api_key"], var.llm_provider_type)
    error_message = "llm_provider_type must be vertex_ai or api_key."
  }

  validation {
    condition = var.llm_provider_type != "api_key" || (
      trimspace(var.gemini_api_key) != "" || length(compact([for key in var.gemini_api_keys : trimspace(key)])) > 0
    )
    error_message = "gemini_api_key or gemini_api_keys must be set when llm_provider_type is api_key."
  }
}

variable "rust_log" {
  description = "RUST_LOG filter string for the app-server container."
  type        = string
  default     = "info,tower_http=debug"
}
