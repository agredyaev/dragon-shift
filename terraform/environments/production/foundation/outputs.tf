output "project_id" {
  description = "GCP project ID."
  value       = var.project_id
}

output "project_number" {
  description = "GCP project number."
  value       = data.google_project.this.number
}

output "region" {
  description = "Primary production region."
  value       = var.region
}

output "cluster_name" {
  description = "GKE cluster name."
  value       = module.gke.name
}

output "cluster_location" {
  description = "GKE cluster region."
  value       = module.gke.location
}

output "cluster_endpoint" {
  description = "GKE control plane endpoint."
  value       = module.gke.endpoint
}

output "cluster_ca_certificate" {
  description = "GKE cluster CA certificate."
  value       = module.gke.ca_certificate
  sensitive   = true
}

output "network_name" {
  description = "VPC network name."
  value       = module.network.network_name
}

output "subnetwork_name" {
  description = "VPC subnet name."
  value       = module.network.subnetwork_name
}

output "cloud_sql_instance_name" {
  description = "Cloud SQL instance name."
  value       = module.cloud_sql.instance_name
}

output "cloud_sql_private_ip_address" {
  description = "Cloud SQL private IP address."
  value       = module.cloud_sql.private_ip_address
}

output "database_name" {
  description = "Application database name."
  value       = module.cloud_sql.database_name
}

output "database_user" {
  description = "Application database user."
  value       = module.cloud_sql.database_user
}

output "database_url_secret_id" {
  description = "Secret Manager secret ID that stores DATABASE_URL."
  value       = google_secret_manager_secret.database_url.secret_id
}

output "cloud_sql_activation_policy" {
  description = "Cloud SQL activation policy."
  value       = module.cloud_sql.activation_policy
}

output "workload_identity_pool" {
  description = "Workload Identity pool for the cluster."
  value       = module.gke.workload_pool
}

output "notification_channel_id" {
  description = "Monitoring notification channel ID."
  value       = google_monitoring_notification_channel.email.name
}
