output "name" {
  description = "GKE cluster name."
  value       = google_container_cluster.this.name
}

output "id" {
  description = "GKE cluster resource ID."
  value       = google_container_cluster.this.id
}

output "endpoint" {
  description = "Public control plane endpoint."
  value       = google_container_cluster.this.endpoint
}

output "ca_certificate" {
  description = "Cluster CA certificate."
  value       = google_container_cluster.this.master_auth[0].cluster_ca_certificate
  sensitive   = true
}

output "location" {
  description = "Cluster region."
  value       = google_container_cluster.this.location
}

output "workload_pool" {
  description = "Workload Identity pool."
  value       = google_container_cluster.this.workload_identity_config[0].workload_pool
}
