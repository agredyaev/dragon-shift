output "enabled_services" {
  description = "Enabled project services."
  value = concat(
    [for service in google_project_service.bootstrap : service.service],
    [for service in google_project_service.services : service.service],
  )
}
