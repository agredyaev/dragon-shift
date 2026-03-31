locals {
  state_bucket_name = var.state_bucket_name != "" ? var.state_bucket_name : format("%s-tfstate", var.project_id)
}

resource "google_storage_bucket" "terraform_state" {
  name                        = local.state_bucket_name
  location                    = var.region
  project                     = var.project_id
  labels                      = var.labels
  uniform_bucket_level_access = true
  force_destroy               = false
  public_access_prevention    = "enforced"

  versioning {
    enabled = true
  }

  lifecycle_rule {
    condition {
      num_newer_versions = 20
    }

    action {
      type = "Delete"
    }
  }

  retention_policy {
    retention_period = 604800
    is_locked        = false
  }
}
