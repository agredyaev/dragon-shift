variable "project_id" {
  description = "GCP project ID that owns the Terraform state bucket."
  type        = string
}

variable "region" {
  description = "Region used for the state bucket location."
  type        = string
}

variable "state_bucket_name" {
  description = "Optional explicit GCS bucket name for Terraform state."
  type        = string
  default     = ""
}

variable "labels" {
  description = "Labels applied to the Terraform state bucket."
  type        = map(string)
  default = {
    managed-by = "terraform"
    stack      = "dragon-shift"
  }
}
