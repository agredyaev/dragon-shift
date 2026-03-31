variable "project_id" {
  description = "GCP project ID."
  type        = string
}

variable "region" {
  description = "Region for regional resources."
  type        = string
}

variable "name" {
  description = "Base name for network resources."
  type        = string
}

variable "network_cidr" {
  description = "CIDR for the primary GKE/application subnet."
  type        = string
}

variable "pods_cidr" {
  description = "Secondary CIDR for GKE pods."
  type        = string
}

variable "services_cidr" {
  description = "Secondary CIDR for GKE services."
  type        = string
}

variable "master_ipv4_cidr_block" {
  description = "Private control plane CIDR for the GKE cluster."
  type        = string
}

variable "cloud_sql_allocated_ip_range" {
  description = "CIDR size base for private service access range."
  type        = number
  default     = 20
}
