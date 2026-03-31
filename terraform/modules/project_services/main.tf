locals {
  bootstrap_services = toset([
    for service in var.services : service
    if service == "serviceusage.googleapis.com"
  ])

  managed_services = toset([
    for service in var.services : service
    if service != "serviceusage.googleapis.com"
  ])
}

resource "google_project_service" "bootstrap" {
  for_each = local.bootstrap_services

  project                    = var.project_id
  service                    = each.value
  disable_on_destroy         = false
  disable_dependent_services = false
}

resource "google_project_service" "services" {
  for_each = local.managed_services

  project                    = var.project_id
  service                    = each.value
  disable_on_destroy         = false
  disable_dependent_services = false

  depends_on = [google_project_service.bootstrap]
}
