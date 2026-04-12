locals {
  state_bucket_name      = var.state_bucket_name != "" ? var.state_bucket_name : format("%s-tfstate", var.project_id)
  github_actions_enabled = trimspace(var.github_repository_id) != "" && trimspace(var.github_repository_owner_id) != ""
  bootstrap_services = toset([
    "cloudresourcemanager.googleapis.com",
    "iam.googleapis.com",
    "iamcredentials.googleapis.com",
    "serviceusage.googleapis.com",
    "storage.googleapis.com",
    "sts.googleapis.com",
  ])
}

data "google_project" "this" {
  project_id = var.project_id
}

resource "google_project_service" "bootstrap" {
  for_each = local.bootstrap_services

  project                    = var.project_id
  service                    = each.value
  disable_on_destroy         = false
  disable_dependent_services = false
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

  depends_on = [google_project_service.bootstrap]
}

resource "google_service_account" "github_actions" {
  count = local.github_actions_enabled ? 1 : 0

  account_id   = var.github_actions_service_account_id
  display_name = var.github_actions_service_account_display_name
  description  = "GitHub Actions Terraform deployer for Dragon Shift"
  project      = var.project_id

  depends_on = [google_project_service.bootstrap]
}

resource "google_iam_workload_identity_pool" "github_actions" {
  count = local.github_actions_enabled ? 1 : 0

  project                   = var.project_id
  workload_identity_pool_id = var.github_actions_workload_identity_pool_id
  display_name              = "GitHub Actions"
  description               = "GitHub Actions OIDC identities for Dragon Shift automation"

  depends_on = [google_project_service.bootstrap]
}

resource "google_iam_workload_identity_pool_provider" "github_actions" {
  count = local.github_actions_enabled ? 1 : 0

  project                            = var.project_id
  workload_identity_pool_id          = google_iam_workload_identity_pool.github_actions[0].workload_identity_pool_id
  workload_identity_pool_provider_id = var.github_actions_workload_identity_provider_id
  display_name                       = "GitHub Actions"
  description                        = "GitHub Actions OIDC provider for Dragon Shift"
  attribute_mapping = {
    "google.subject"                = "assertion.sub"
    "attribute.repository_id"       = "assertion.repository_id"
    "attribute.repository_owner"    = "assertion.repository_owner"
    "attribute.repository_owner_id" = "assertion.repository_owner_id"
    "attribute.ref"                 = "assertion.ref"
  }
  attribute_condition = format(
    "assertion.repository_owner_id=='%s' && assertion.repository_id=='%s' && assertion.ref=='%s' && assertion.event_name=='%s' && assertion.workflow_ref=='%s'",
    var.github_repository_owner_id,
    var.github_repository_id,
    var.github_actions_allowed_ref,
    var.github_actions_allowed_event_name,
    var.github_actions_allowed_workflow_ref,
  )

  oidc {
    issuer_uri = "https://token.actions.githubusercontent.com"
  }

  depends_on = [google_project_service.bootstrap]
}

resource "google_service_account_iam_member" "github_actions_workload_identity_user" {
  count = local.github_actions_enabled ? 1 : 0

  service_account_id = google_service_account.github_actions[0].name
  role               = "roles/iam.workloadIdentityUser"
  member = format(
    "principalSet://iam.googleapis.com/projects/%s/locations/global/workloadIdentityPools/%s/attribute.repository_id/%s",
    data.google_project.this.number,
    google_iam_workload_identity_pool.github_actions[0].workload_identity_pool_id,
    var.github_repository_id,
  )
}

resource "google_service_account_iam_member" "github_actions_token_creator" {
  count = local.github_actions_enabled ? 1 : 0

  service_account_id = google_service_account.github_actions[0].name
  role               = "roles/iam.serviceAccountTokenCreator"
  member = format(
    "principalSet://iam.googleapis.com/projects/%s/locations/global/workloadIdentityPools/%s/attribute.repository_id/%s",
    data.google_project.this.number,
    google_iam_workload_identity_pool.github_actions[0].workload_identity_pool_id,
    var.github_repository_id,
  )
}

resource "google_project_iam_member" "github_actions_roles" {
  for_each = local.github_actions_enabled ? var.github_actions_service_account_roles : toset([])

  project = var.project_id
  role    = each.value
  member  = format("serviceAccount:%s", google_service_account.github_actions[0].email)
}

resource "google_storage_bucket_iam_member" "github_actions_state_bucket_roles" {
  for_each = local.github_actions_enabled ? var.github_actions_state_bucket_roles : toset([])

  bucket = google_storage_bucket.terraform_state.name
  role   = each.value
  member = format("serviceAccount:%s", google_service_account.github_actions[0].email)
}
