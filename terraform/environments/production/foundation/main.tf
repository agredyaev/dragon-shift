locals {
  common_labels = merge(
    {
      environment = var.environment
      managed-by  = "terraform"
      service     = "dragon-shift"
    },
    var.labels,
  )

  required_services = toset([
    "cloudresourcemanager.googleapis.com",
    "compute.googleapis.com",
    "container.googleapis.com",
    "dns.googleapis.com",
    "iam.googleapis.com",
    "logging.googleapis.com",
    "monitoring.googleapis.com",
    "secretmanager.googleapis.com",
    "servicenetworking.googleapis.com",
    "serviceusage.googleapis.com",
    "sqladmin.googleapis.com",
  ])

  database_url_secret_payload = format(
    "postgres://%s:%s@%s:5432/%s",
    urlencode(module.cloud_sql.database_user),
    urlencode(var.db_password),
    module.cloud_sql.private_ip_address,
    module.cloud_sql.database_name,
  )

  hex_digit_values = {
    "0" = 0
    "1" = 1
    "2" = 2
    "3" = 3
    "4" = 4
    "5" = 5
    "6" = 6
    "7" = 7
    "8" = 8
    "9" = 9
    "a" = 10
    "b" = 11
    "c" = 12
    "d" = 13
    "e" = 14
    "f" = 15
  }

  database_url_secret_payload_hash_prefix = substr(md5(local.database_url_secret_payload), 0, 8)

  database_url_secret_payload_version = (
    local.hex_digit_values[substr(local.database_url_secret_payload_hash_prefix, 0, 1)] * 268435456 +
    local.hex_digit_values[substr(local.database_url_secret_payload_hash_prefix, 1, 1)] * 16777216 +
    local.hex_digit_values[substr(local.database_url_secret_payload_hash_prefix, 2, 1)] * 1048576 +
    local.hex_digit_values[substr(local.database_url_secret_payload_hash_prefix, 3, 1)] * 65536 +
    local.hex_digit_values[substr(local.database_url_secret_payload_hash_prefix, 4, 1)] * 4096 +
    local.hex_digit_values[substr(local.database_url_secret_payload_hash_prefix, 5, 1)] * 256 +
    local.hex_digit_values[substr(local.database_url_secret_payload_hash_prefix, 6, 1)] * 16 +
    local.hex_digit_values[substr(local.database_url_secret_payload_hash_prefix, 7, 1)]
  ) + var.database_url_secret_version
}

data "google_project" "this" {
  project_id = var.project_id
}

module "project_services" {
  source = "../../../modules/project_services"

  project_id = var.project_id
  services   = local.required_services
}

module "network" {
  source = "../../../modules/network"

  project_id                   = var.project_id
  region                       = var.region
  name                         = var.network_name
  network_cidr                 = var.network_cidr
  pods_cidr                    = var.pods_cidr
  services_cidr                = var.services_cidr
  master_ipv4_cidr_block       = var.master_ipv4_cidr_block
  cloud_sql_allocated_ip_range = 20

  depends_on = [module.project_services]
}

module "gke" {
  source = "../../../modules/gke_autopilot"

  project_id                    = var.project_id
  name                          = var.cluster_name
  region                        = var.region
  network                       = module.network.network_name
  subnetwork                    = module.network.subnetwork_name
  pods_secondary_range_name     = module.network.pods_secondary_range_name
  services_secondary_range_name = module.network.services_secondary_range_name
  master_ipv4_cidr_block        = var.master_ipv4_cidr_block
  release_channel               = var.release_channel
  master_authorized_networks    = var.master_authorized_networks

  depends_on = [module.project_services]
}

module "cloud_sql" {
  source = "../../../modules/cloud_sql"

  project_id                = var.project_id
  region                    = var.region
  name                      = var.db_instance_name
  network_id                = module.network.network_id
  database_name             = var.db_name
  database_user             = var.db_user
  database_password         = var.db_password
  database_password_version = var.db_password_version
  tier                      = var.db_tier
  disk_size_gb              = var.db_disk_size_gb
  labels                    = local.common_labels

  depends_on = [module.project_services, module.network]
}

resource "google_secret_manager_secret" "database_url" {
  secret_id = "dragon-shift-production-database-url"
  project   = var.project_id

  replication {
    auto {}
  }

  labels = local.common_labels

  depends_on = [module.project_services]
}

resource "google_secret_manager_secret_version" "database_url" {
  secret                 = google_secret_manager_secret.database_url.id
  secret_data_wo         = local.database_url_secret_payload
  secret_data_wo_version = local.database_url_secret_payload_version
}

resource "google_monitoring_notification_channel" "email" {
  display_name = "Dragon Shift Production Email"
  type         = "email"
  project      = var.project_id

  labels = {
    email_address = var.support_email
  }

  user_labels = local.common_labels

  depends_on = [module.project_services]
}
