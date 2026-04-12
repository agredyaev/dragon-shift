variable "project_id" {
  description = "GCP project ID that owns the Terraform state bucket."
  type        = string
}

variable "region" {
  description = "Region used for the state bucket location."
  type        = string
}

variable "state_bucket_name" {
  description = "Optional explicit GCS bucket name for Terraform state."
  type        = string
  default     = ""
}

variable "labels" {
  description = "Labels applied to the Terraform state bucket."
  type        = map(string)
  default = {
    managed-by = "terraform"
    stack      = "dragon-shift"
  }
}

variable "github_repository_id" {
  description = "Numeric GitHub repository ID allowed to use Workload Identity Federation. Leave empty to skip GitHub bootstrap resources."
  type        = string
  default     = ""
}

variable "github_repository_owner_id" {
  description = "Numeric GitHub repository owner ID allowed to use Workload Identity Federation. Leave empty to skip GitHub bootstrap resources."
  type        = string
  default     = ""
}

variable "github_actions_workload_identity_pool_id" {
  description = "Workload Identity Pool ID for GitHub Actions."
  type        = string
  default     = "github-actions"
}

variable "github_actions_workload_identity_provider_id" {
  description = "Workload Identity Provider ID for GitHub Actions."
  type        = string
  default     = "github"
}

variable "github_actions_allowed_ref" {
  description = "Git ref allowed to assume the GitHub Actions Terraform service account."
  type        = string
  default     = "refs/heads/main"
}

variable "github_actions_allowed_event_name" {
  description = "GitHub Actions event name allowed to assume the Terraform service account."
  type        = string
  default     = "push"
}

variable "github_actions_allowed_workflow_ref" {
  description = "Workflow ref allowed to assume the Terraform service account."
  type        = string
  default     = "agredyaev/dragon-shift/.github/workflows/publish-image.yml@refs/heads/main"
}

variable "github_actions_service_account_id" {
  description = "Service account ID used by GitHub Actions Terraform automation."
  type        = string
  default     = "github-terraform"
}

variable "github_actions_service_account_display_name" {
  description = "Display name for the GitHub Actions Terraform service account."
  type        = string
  default     = "GitHub Terraform Deploy"
}

variable "github_actions_service_account_roles" {
  description = "Project roles granted to the GitHub Actions Terraform service account."
  type        = set(string)
  default = [
    "roles/cloudsql.admin",
    "roles/compute.admin",
    "roles/container.admin",
    "roles/dns.admin",
    "roles/monitoring.alertPolicyEditor",
    "roles/monitoring.notificationChannelEditor",
    "roles/monitoring.uptimeCheckConfigEditor",
    "roles/secretmanager.admin",
    "roles/serviceusage.serviceUsageAdmin",
  ]
}

variable "github_actions_state_bucket_roles" {
  description = "Bucket roles granted to the GitHub Actions Terraform service account for the Terraform state bucket."
  type        = set(string)
  default = [
    "roles/storage.bucketViewer",
    "roles/storage.objectAdmin",
  ]
}
