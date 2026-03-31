variable "project_id" {
  description = "GCP project ID."
  type        = string
}

variable "services" {
  description = "APIs that must be enabled for the project."
  type        = set(string)
}
