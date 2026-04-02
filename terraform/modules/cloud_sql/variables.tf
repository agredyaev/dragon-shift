variable "project_id" {
  description = "GCP project ID."
  type        = string
}

variable "region" {
  description = "Cloud SQL region."
  type        = string
}

variable "name" {
  description = "Cloud SQL instance name."
  type        = string
}

variable "database_version" {
  description = "Cloud SQL Postgres version."
  type        = string
  default     = "POSTGRES_16"
}

variable "tier" {
  description = "Cloud SQL machine tier."
  type        = string
  default     = "db-custom-2-7680"
}

variable "availability_type" {
  description = "Cloud SQL availability type."
  type        = string
  default     = "REGIONAL"
}

variable "activation_policy" {
  description = "Cloud SQL activation policy. Use ALWAYS for normal runtime and NEVER to stop instance compute while retaining data."
  type        = string
  default     = "ALWAYS"
}

variable "network_id" {
  description = "VPC network self link for private IP."
  type        = string
}

variable "database_name" {
  description = "Application database name."
  type        = string
}

variable "database_user" {
  description = "Application database username."
  type        = string
}

variable "database_password" {
  description = "Application database password."
  type        = string
  sensitive   = true
}

variable "database_password_version" {
  description = "Write-only version counter for the application database password. Increment to rotate the password."
  type        = number
  default     = 1
}

variable "deletion_protection" {
  description = "Protect the Cloud SQL instance from accidental deletion."
  type        = bool
  default     = true
}

variable "backup_start_time" {
  description = "Daily backup start time in UTC."
  type        = string
  default     = "02:00"
}

variable "maintenance_day" {
  description = "Maintenance day, 1-7 (Mon-Sun)."
  type        = number
  default     = 7
}

variable "maintenance_hour" {
  description = "Maintenance hour in UTC."
  type        = number
  default     = 3
}

variable "disk_size_gb" {
  description = "Initial disk size in GB."
  type        = number
  default     = 50
}

variable "labels" {
  description = "Labels for Cloud SQL resources."
  type        = map(string)
  default     = {}
}
