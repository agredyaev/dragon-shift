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
    "aiplatform.googleapis.com",
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
  activation_policy         = var.db_activation_policy
  tier                      = var.db_tier
  disk_size_gb              = var.db_disk_size_gb
  labels                    = local.common_labels

  depends_on = [module.project_services, module.network]
}

resource "google_secret_manager_secret" "database_url" {
  secret_id = var.database_url_secret_id
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
  secret_data_wo_version = var.database_url_secret_version
}

resource "terraform_data" "ensure_session_cookie_key_secret" {
  input = {
    project_id = var.project_id
    secret_id  = var.session_cookie_key_secret_id
  }

  provisioner "local-exec" {
    interpreter = ["/bin/bash", "-c"]

    environment = {
      PROJECT_ID = var.project_id
      SECRET_ID  = var.session_cookie_key_secret_id
    }

    command = <<-EOT
      set -euo pipefail

      if ! gcloud secrets describe "$SECRET_ID" --project "$PROJECT_ID" >/dev/null 2>&1; then
        gcloud secrets create "$SECRET_ID" \
          --project "$PROJECT_ID" \
          --replication-policy=automatic >/dev/null
      fi

      if [[ -z "$(gcloud secrets versions list "$SECRET_ID" --project "$PROJECT_ID" --filter='state=ENABLED' --limit=1 --format='value(name)')" ]]; then
        openssl rand -base64 64 | tr -d '\n' | gcloud secrets versions add "$SECRET_ID" --project "$PROJECT_ID" --data-file=- >/dev/null
      fi
    EOT
  }

  depends_on = [module.project_services]
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
