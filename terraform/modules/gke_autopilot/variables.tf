variable "project_id" {
  description = "GCP project ID."
  type        = string
}

variable "name" {
  description = "Cluster name."
  type        = string
}

variable "region" {
  description = "Cluster region."
  type        = string
}

variable "network" {
  description = "VPC network name or self link."
  type        = string
}

variable "subnetwork" {
  description = "Subnetwork name or self link."
  type        = string
}

variable "pods_secondary_range_name" {
  description = "Secondary range name for pods."
  type        = string
}

variable "services_secondary_range_name" {
  description = "Secondary range name for services."
  type        = string
}

variable "master_ipv4_cidr_block" {
  description = "Private control plane CIDR."
  type        = string
}

variable "release_channel" {
  description = "GKE release channel."
  type        = string
  default     = "REGULAR"
}

variable "maintenance_start_time" {
  description = "Daily UTC maintenance window start time in RFC3339 partial format, e.g. 03:00."
  type        = string
  default     = "03:00"
}

variable "maintenance_end_time" {
  description = "Daily UTC maintenance window end time in RFC3339 partial format, e.g. 07:00."
  type        = string
  default     = "07:00"
}

variable "maintenance_exclusions" {
  description = "Optional maintenance exclusions."
  type = list(object({
    name       = string
    start_time = string
    end_time   = string
    scope      = string
  }))
  default = []
}

variable "master_authorized_networks" {
  description = "Optional authorized CIDR blocks for the public GKE control plane endpoint."
  type = list(object({
    cidr_block   = string
    display_name = string
  }))
}
