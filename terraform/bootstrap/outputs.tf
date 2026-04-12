output "state_bucket_name" {
  description = "Name of the GCS bucket used for Terraform state."
  value       = google_storage_bucket.terraform_state.name
}

output "github_actions_service_account_email" {
  description = "Service account email used by GitHub Actions Terraform automation."
  value       = local.github_actions_enabled ? google_service_account.github_actions[0].email : ""
}

output "github_actions_workload_identity_provider" {
  description = "Full resource name of the GitHub Actions Workload Identity Provider."
  value       = local.github_actions_enabled ? google_iam_workload_identity_pool_provider.github_actions[0].name : ""
}
