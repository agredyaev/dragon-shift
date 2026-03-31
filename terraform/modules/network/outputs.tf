output "network_name" {
  description = "VPC network name."
  value       = google_compute_network.this.name
}

output "network_id" {
  description = "VPC network self link."
  value       = google_compute_network.this.id
}

output "subnetwork_name" {
  description = "Primary subnet name."
  value       = google_compute_subnetwork.primary.name
}

output "subnetwork_id" {
  description = "Primary subnet self link."
  value       = google_compute_subnetwork.primary.id
}

output "pods_secondary_range_name" {
  description = "Secondary range name for pods."
  value       = google_compute_subnetwork.primary.secondary_ip_range[0].range_name
}

output "services_secondary_range_name" {
  description = "Secondary range name for services."
  value       = google_compute_subnetwork.primary.secondary_ip_range[1].range_name
}

output "private_service_access_connection" {
  description = "Private service access connection resource name."
  value       = google_service_networking_connection.private_service_access.id
}

output "master_ipv4_cidr_block" {
  description = "Reserved private control plane CIDR."
  value       = var.master_ipv4_cidr_block
}
