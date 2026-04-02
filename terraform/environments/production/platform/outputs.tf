output "hostname" {
  description = "Production hostname."
  value       = local.app_hostname
}

output "global_ip_address" {
  description = "Reserved global IP address for the ingress."
  value       = google_compute_global_address.ingress.address
}

output "dns_name_servers" {
  description = "Authoritative name servers for the created public DNS zone."
  value       = local.managed_dns_enabled ? google_dns_managed_zone.public[0].name_servers : []
}

output "verify_url" {
  description = "Public base URL for deploy verification and smoke checks."
  value       = format("https://%s", local.app_hostname)
}

output "namespace" {
  description = "Kubernetes namespace used by the app."
  value       = kubernetes_namespace.app.metadata[0].name
}

output "helm_release_name" {
  description = "Helm release name."
  value       = helm_release.app.name
}

output "database_secret_provider_class_name" {
  description = "SecretProviderClass name used to mount DATABASE_URL from Secret Manager."
  value       = kubernetes_manifest.database_secret_provider_class.manifest.metadata.name
}

output "cloud_armor_policy_name" {
  description = "Cloud Armor policy attached to the ingress backend."
  value       = var.enable_cloud_armor ? google_compute_security_policy.app[0].name : ""
}
