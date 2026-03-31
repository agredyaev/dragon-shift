terraform {
  required_version = ">= 1.14.0"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 6.31"
    }

    kubernetes = {
      source  = "hashicorp/kubernetes"
      version = "~> 2.36"
    }

    helm = {
      source  = "hashicorp/helm"
      version = "~> 2.15"
    }
  }

  backend "gcs" {}
}

provider "google" {
  project = var.project_id
  region  = var.region
}

data "google_client_config" "this" {}

data "google_project" "this" {
  project_id = var.project_id
}

data "google_container_cluster" "this" {
  project  = var.project_id
  name     = var.cluster_name
  location = trimspace(var.cluster_location) != "" ? var.cluster_location : var.region
}

locals {
  use_kubeconfig = trimspace(var.kubeconfig_path) != ""
}

provider "kubernetes" {
  config_path            = local.use_kubeconfig ? var.kubeconfig_path : null
  config_context         = local.use_kubeconfig && trimspace(var.kubeconfig_context) != "" ? var.kubeconfig_context : null
  host                   = local.use_kubeconfig ? null : format("https://%s", data.google_container_cluster.this.endpoint)
  token                  = local.use_kubeconfig ? null : data.google_client_config.this.access_token
  cluster_ca_certificate = local.use_kubeconfig ? null : base64decode(data.google_container_cluster.this.master_auth[0].cluster_ca_certificate)
}

provider "helm" {
  kubernetes {
    config_path            = local.use_kubeconfig ? var.kubeconfig_path : null
    config_context         = local.use_kubeconfig && trimspace(var.kubeconfig_context) != "" ? var.kubeconfig_context : null
    host                   = local.use_kubeconfig ? null : format("https://%s", data.google_container_cluster.this.endpoint)
    token                  = local.use_kubeconfig ? null : data.google_client_config.this.access_token
    cluster_ca_certificate = local.use_kubeconfig ? null : base64decode(data.google_container_cluster.this.master_auth[0].cluster_ca_certificate)
  }
}
