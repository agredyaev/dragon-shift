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
  description = "GKE cluster name."
  type        = string
  default     = "dragon-shift-prod"
}

variable "network_name" {
  description = "Base name for networking resources."
  type        = string
  default     = "dragon-shift-prod"
}

variable "network_cidr" {
  description = "Primary subnet CIDR."
  type        = string
  default     = "10.20.0.0/20"
}

variable "pods_cidr" {
  description = "Pods secondary CIDR."
  type        = string
  default     = "10.24.0.0/14"
}

variable "services_cidr" {
  description = "Services secondary CIDR."
  type        = string
  default     = "10.28.0.0/20"
}

variable "db_instance_name" {
  description = "Cloud SQL instance name."
  type        = string
  default     = "dragon-shift-prod-pg"
}

variable "db_name" {
  description = "Application database name."
  type        = string
  default     = "dragon_shift"
}

variable "db_user" {
  description = "Application database user."
  type        = string
  default     = "dragon_shift"
}

variable "db_password" {
  description = "Application database password."
  type        = string
  sensitive   = true
}

variable "db_password_version" {
  description = "Write-only version counter for the application database password. Increment to rotate the password."
  type        = number
  default     = 1
}

variable "database_url_secret_version" {
  description = "Additional write-only version counter for the Secret Manager DATABASE_URL secret payload. Increment to force an extra rotation beyond automatic payload-change rotations."
  type        = number
  default     = 1
}

variable "db_tier" {
  description = "Cloud SQL machine tier."
  type        = string
  default     = "db-custom-2-7680"
}

variable "db_disk_size_gb" {
  description = "Initial Cloud SQL disk size in GB."
  type        = number
  default     = 50
}

variable "db_activation_policy" {
  description = "Cloud SQL activation policy. Set to NEVER to pause instance compute while retaining data."
  type        = string
  default     = "ALWAYS"
}

variable "database_url_secret_id" {
  description = "Secret Manager secret ID that stores the runtime DATABASE_URL."
  type        = string
  default     = "dragon-shift-production-database-url"
}

variable "release_channel" {
  description = "GKE release channel."
  type        = string
  default     = "REGULAR"
}

variable "master_authorized_networks" {
  description = "Authorized CIDR blocks for the GKE public control plane endpoint used by operators or CI runners."
  type = list(object({
    cidr_block   = string
    display_name = string
  }))

  validation {
    condition     = length(var.master_authorized_networks) > 0
    error_message = "master_authorized_networks must include at least one operator or CI egress CIDR so the platform stack can reach the GKE control plane."
  }
}

variable "support_email" {
  description = "Primary operator email for alerting."
  type        = string
}

variable "labels" {
  description = "Common resource labels."
  type        = map(string)
  default     = {}
}
