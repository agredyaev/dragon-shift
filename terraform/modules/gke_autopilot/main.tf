resource "google_container_cluster" "this" {
  name     = var.name
  project  = var.project_id
  location = var.region

  enable_autopilot = true

  network    = var.network
  subnetwork = var.subnetwork

  deletion_protection = true

  release_channel {
    channel = var.release_channel
  }

  ip_allocation_policy {
    cluster_secondary_range_name  = var.pods_secondary_range_name
    services_secondary_range_name = var.services_secondary_range_name
  }

  private_cluster_config {
    enable_private_nodes    = true
    enable_private_endpoint = false
    master_ipv4_cidr_block  = var.master_ipv4_cidr_block
  }

  master_authorized_networks_config {
    gcp_public_cidrs_access_enabled = false

    dynamic "cidr_blocks" {
      for_each = var.master_authorized_networks
      content {
        cidr_block   = cidr_blocks.value.cidr_block
        display_name = cidr_blocks.value.display_name
      }
    }
  }

  secret_manager_config {
    enabled = true
  }

  maintenance_policy {
    recurring_window {
      start_time = format("1970-01-01T%s:00Z", var.maintenance_start_time)
      end_time   = format("1970-01-01T%s:00Z", var.maintenance_end_time)
      recurrence = "FREQ=DAILY"
    }

    dynamic "maintenance_exclusion" {
      for_each = var.maintenance_exclusions
      content {
        exclusion_name = maintenance_exclusion.value.name
        start_time     = maintenance_exclusion.value.start_time
        end_time       = maintenance_exclusion.value.end_time

        exclusion_options {
          scope = maintenance_exclusion.value.scope
        }
      }
    }
  }

  monitoring_config {
    enable_components = [
      "APISERVER",
      "CONTROLLER_MANAGER",
      "SCHEDULER",
      "SYSTEM_COMPONENTS",
      "WORKLOADS",
    ]

    managed_prometheus {
      enabled = true
    }
  }

  logging_config {
    enable_components = [
      "APISERVER",
      "CONTROLLER_MANAGER",
      "SCHEDULER",
      "SYSTEM_COMPONENTS",
      "WORKLOADS",
    ]
  }

  workload_identity_config {
    workload_pool = format("%s.svc.id.goog", var.project_id)
  }
}
