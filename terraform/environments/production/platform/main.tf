locals {
  notification_channel_id = trimspace(var.notification_channel_id)
  managed_dns_enabled     = var.hostname_mode == "managed_dns"
  app_hostname            = var.hostname_mode == "nip_io" ? format("%s.%s.nip.io", trimspace(var.nip_io_label), google_compute_global_address.ingress.address) : trimspace(var.hostname)

  common_labels = merge(
    {
      environment = var.environment
      managed-by  = "terraform"
      service     = "dragon-shift"
    },
    var.labels,
  )

  database_secret_mount_path = "/var/run/secrets/dragon-shift"
  database_secret_file_name  = "DATABASE_URL"
  ksa_name                   = "dragon-shift-app"
  use_image_digest           = trimspace(var.image_digest) != ""
}

check "uptime_alert_channel_configured" {
  assert {
    condition     = !var.enable_uptime_checks || local.notification_channel_id != ""
    error_message = "notification_channel_id must be set when enable_uptime_checks=true."
  }
}

check "hostname_mode_supported" {
  assert {
    condition     = contains(["managed_dns", "external_dns", "nip_io"], var.hostname_mode)
    error_message = "hostname_mode must be one of: managed_dns, external_dns, nip_io."
  }
}

check "hostname_present_when_required" {
  assert {
    condition     = var.hostname_mode == "nip_io" || trimspace(var.hostname) != ""
    error_message = "hostname must be set when hostname_mode is managed_dns or external_dns."
  }
}

check "managed_dns_inputs_present" {
  assert {
    condition     = var.hostname_mode != "managed_dns" || (trimspace(var.dns_zone_name) != "" && trimspace(var.dns_zone_dns_name) != "")
    error_message = "dns_zone_name and dns_zone_dns_name must be set when hostname_mode=managed_dns."
  }
}

check "nip_io_label_present" {
  assert {
    condition     = var.hostname_mode != "nip_io" || trimspace(var.nip_io_label) != ""
    error_message = "nip_io_label must be set when hostname_mode=nip_io."
  }
}

resource "google_compute_global_address" "ingress" {
  project    = var.project_id
  name       = "dragon-shift-production-ip"
  ip_version = "IPV4"
}

resource "google_compute_security_policy" "app" {
  count = var.enable_cloud_armor ? 1 : 0

  project = var.project_id
  name    = "dragon-shift-production"

  rule {
    priority    = 1000
    description = "Throttle abusive clients"
    action      = "throttle"

    match {
      versioned_expr = "SRC_IPS_V1"

      config {
        src_ip_ranges = ["*"]
      }
    }

    rate_limit_options {
      conform_action = "allow"
      exceed_action  = "deny(429)"

      enforce_on_key = "IP"

      rate_limit_threshold {
        count        = var.cloud_armor_rate_limit_count
        interval_sec = var.cloud_armor_rate_limit_interval_sec
      }
    }
  }

  rule {
    priority    = 2147483647
    description = "Default allow"
    action      = "allow"

    match {
      versioned_expr = "SRC_IPS_V1"

      config {
        src_ip_ranges = ["*"]
      }
    }
  }
}

resource "google_dns_managed_zone" "public" {
  count = local.managed_dns_enabled ? 1 : 0

  project       = var.project_id
  name          = var.dns_zone_name
  dns_name      = var.dns_zone_dns_name
  description   = "Dragon Shift production public zone"
  force_destroy = false

  labels = local.common_labels
}

resource "google_dns_record_set" "app" {
  count = local.managed_dns_enabled ? 1 : 0

  project      = var.project_id
  managed_zone = google_dns_managed_zone.public[0].name
  name         = format("%s.", local.app_hostname)
  type         = "A"
  ttl          = 300
  rrdatas      = [google_compute_global_address.ingress.address]
}

resource "kubernetes_namespace" "app" {
  metadata {
    name = var.namespace

    labels = {
      "app.kubernetes.io/part-of" = "dragon-shift"
      environment                 = var.environment
    }
  }

  depends_on = [terraform_data.wait_for_cluster_apis]
}

resource "kubernetes_service_account" "app" {
  metadata {
    name      = local.ksa_name
    namespace = kubernetes_namespace.app.metadata[0].name
    labels = {
      "app.kubernetes.io/name" = "dragon-shift"
    }
  }

  automount_service_account_token = false

  depends_on = [kubernetes_namespace.app, terraform_data.wait_for_cluster_apis]
}

resource "google_secret_manager_secret_iam_member" "database_url_accessor" {
  project   = var.project_id
  secret_id = var.database_url_secret_id
  role      = "roles/secretmanager.secretAccessor"
  member = format(
    "principal://iam.googleapis.com/projects/%s/locations/global/workloadIdentityPools/%s.svc.id.goog/subject/ns/%s/sa/%s",
    data.google_project.this.number,
    var.project_id,
    kubernetes_namespace.app.metadata[0].name,
    kubernetes_service_account.app.metadata[0].name,
  )
}

resource "terraform_data" "wait_for_cluster_apis" {
  triggers_replace = {
    cluster_endpoint = data.google_container_cluster.this.endpoint
    cluster_ca       = data.google_container_cluster.this.master_auth[0].cluster_ca_certificate
    kubeconfig_path  = var.kubeconfig_path
    kubeconfig_ctx   = var.kubeconfig_context
  }

  provisioner "local-exec" {
    interpreter = ["/bin/sh", "-c"]

    environment = {
      CLUSTER_ENDPOINT = format("https://%s", data.google_container_cluster.this.endpoint)
      CLUSTER_CA_CERT  = base64decode(data.google_container_cluster.this.master_auth[0].cluster_ca_certificate)
      CLUSTER_TOKEN    = data.google_client_config.this.access_token
      KUBECONFIG_PATH  = var.kubeconfig_path
      KUBECONFIG_CTX   = var.kubeconfig_context
    }

    command = <<-EOT
      set -eu

      ca_file="$(mktemp)"
      trap 'rm -f "$ca_file"' EXIT
      printf '%s' "$CLUSTER_CA_CERT" > "$ca_file"

      kubectl_args=""
      if [ -n "$KUBECONFIG_PATH" ]; then
        kubectl_args="--kubeconfig=$KUBECONFIG_PATH"
        if [ -n "$KUBECONFIG_CTX" ]; then
          kubectl_args="$kubectl_args --context=$KUBECONFIG_CTX"
        fi
      fi

      wait_for_api() {
        path="$1"
        name="$2"
        attempt=1
        while [ "$attempt" -le 60 ]; do
          if [ -n "$KUBECONFIG_PATH" ]; then
            if kubectl $kubectl_args get --raw "/$path" >/dev/null 2>&1; then
              return 0
            fi
          else
            status_code="$(curl --silent --output /dev/null --write-out '%%{http_code}' --cacert "$ca_file" --header "Authorization: Bearer $CLUSTER_TOKEN" "$CLUSTER_ENDPOINT/$path" || true)"
            if [ "$status_code" = "200" ]; then
              return 0
            fi
          fi
          sleep 5
          attempt=$((attempt + 1))
        done

        printf 'timed out waiting for %s at %s\n' "$name" "$path" >&2
        exit 1
      }

      wait_for_api readyz "Kubernetes API readiness"
      wait_for_api apis/cloud.google.com/v1 "BackendConfig API"
      wait_for_api apis/networking.gke.io/v1 "ManagedCertificate API"
      wait_for_api apis/secrets-store.csi.x-k8s.io/v1 "SecretProviderClass API"
      wait_for_api apis/storage.k8s.io/v1/csidrivers/secrets-store-gke.csi.k8s.io "Secret Manager CSI driver"
    EOT
  }
}

resource "kubernetes_manifest" "database_secret_provider_class" {
  manifest = {
    apiVersion = "secrets-store.csi.x-k8s.io/v1"
    kind       = "SecretProviderClass"
    metadata = {
      name      = "dragon-shift-database-url"
      namespace = kubernetes_namespace.app.metadata[0].name
    }
    spec = {
      provider = "gke"
      parameters = {
        secrets = <<-EOT
          - resourceName: "projects/${var.project_id}/secrets/${var.database_url_secret_id}/versions/latest"
            path: "${local.database_secret_file_name}"
        EOT
      }
    }
  }

  depends_on = [kubernetes_namespace.app, terraform_data.wait_for_cluster_apis]
}

resource "kubernetes_manifest" "backend_config" {
  manifest = {
    apiVersion = "cloud.google.com/v1"
    kind       = "BackendConfig"
    metadata = {
      name      = "dragon-shift-backend-config"
      namespace = kubernetes_namespace.app.metadata[0].name
    }
    spec = merge(
      {
        timeoutSec = 3600
        healthCheck = {
          type               = "HTTP"
          requestPath        = "/api/ready"
          port               = 3000
          checkIntervalSec   = 15
          timeoutSec         = 5
          healthyThreshold   = 2
          unhealthyThreshold = 3
        }
        connectionDraining = {
          drainingTimeoutSec = 30
        }
      },
      var.enable_cloud_armor ? {
        securityPolicy = {
          name = google_compute_security_policy.app[0].name
        }
      } : {},
    )
  }

  depends_on = [
    kubernetes_namespace.app,
    terraform_data.wait_for_cluster_apis,
    google_compute_security_policy.app,
  ]
}

resource "kubernetes_manifest" "managed_certificate" {
  manifest = {
    apiVersion = "networking.gke.io/v1"
    kind       = "ManagedCertificate"
    metadata = {
      name      = "dragon-shift-managed-cert"
      namespace = kubernetes_namespace.app.metadata[0].name
    }
    spec = {
      domains = [local.app_hostname]
    }
  }

  depends_on = [kubernetes_namespace.app, terraform_data.wait_for_cluster_apis]
}

resource "helm_release" "app" {
  name              = var.release_name
  namespace         = kubernetes_namespace.app.metadata[0].name
  repository        = null
  chart             = var.helm_chart_path
  dependency_update = true
  atomic            = true
  cleanup_on_fail   = true
  timeout           = 900
  wait              = true

  values = [yamlencode({
    image = {
      repository = var.image_repository
      digest     = local.use_image_digest ? var.image_digest : ""
      tag        = local.use_image_digest ? "ignored" : var.image_tag
    }
    ingress = {
      enabled   = true
      className = "gce"
      host      = local.app_hostname
      annotations = {
        "kubernetes.io/ingress.global-static-ip-name" = google_compute_global_address.ingress.name
        "networking.gke.io/managed-certificates"      = kubernetes_manifest.managed_certificate.manifest.metadata.name
        "kubernetes.io/ingress.allow-http"            = "false"
      }
      tls = {
        enabled    = false
        secretName = ""
      }
    }
    service = {
      type       = "ClusterIP"
      port       = 80
      targetPort = 3000
      annotations = {
        "cloud.google.com/backend-config" = jsonencode({ default = kubernetes_manifest.backend_config.manifest.metadata.name })
        "cloud.google.com/neg"            = jsonencode({ ingress = true })
      }
    }
    app = {
      allowedOrigins        = format("https://%s", local.app_hostname)
      viteAppUrl            = format("https://%s", local.app_hostname)
      rustSessionCodePrefix = var.rust_session_code_prefix
      trustForwardedFor     = var.trust_forwarded_for
      databasePoolSize      = var.database_pool_size
      createRateLimitMax    = var.create_rate_limit_max
      joinRateLimitMax      = var.join_rate_limit_max
      commandRateLimitMax   = var.command_rate_limit_max
      socketRateLimitMax    = var.websocket_rate_limit_max
    }
    database = {
      existingSecretFile = format("%s/%s", local.database_secret_mount_path, local.database_secret_file_name)
    }
    secretManager = {
      enabled                 = true
      secretProviderClassName = kubernetes_manifest.database_secret_provider_class.manifest.metadata.name
      mountPath               = local.database_secret_mount_path
      fileName                = local.database_secret_file_name
    }
    postgresql = {
      enabled = false
    }
    replicaCount = 1
    serviceAccount = {
      create                       = false
      name                         = kubernetes_service_account.app.metadata[0].name
      automountServiceAccountToken = false
    }
    podDisruptionBudget = {
      enabled      = true
      minAvailable = 1
    }
  })]

  depends_on = [
    kubernetes_manifest.backend_config,
    kubernetes_manifest.database_secret_provider_class,
    kubernetes_manifest.managed_certificate,
    kubernetes_service_account.app,
    google_secret_manager_secret_iam_member.database_url_accessor,
    terraform_data.wait_for_cluster_apis,
  ]
}

resource "google_monitoring_uptime_check_config" "live" {
  count = var.enable_uptime_checks ? 1 : 0

  project      = var.project_id
  display_name = "Dragon Shift Production Live"
  timeout      = "10s"
  period       = "60s"

  monitored_resource {
    type = "uptime_url"
    labels = {
      host       = local.app_hostname
      project_id = var.project_id
    }
  }

  http_check {
    path           = "/api/live"
    port           = 443
    use_ssl        = true
    validate_ssl   = true
    request_method = "GET"
  }

  selected_regions = [
    "EUROPE",
    "USA",
  ]
}

resource "google_monitoring_uptime_check_config" "ready" {
  count = var.enable_uptime_checks ? 1 : 0

  project      = var.project_id
  display_name = "Dragon Shift Production Ready"
  timeout      = "10s"
  period       = "60s"

  monitored_resource {
    type = "uptime_url"
    labels = {
      host       = local.app_hostname
      project_id = var.project_id
    }
  }

  http_check {
    path           = "/api/ready"
    port           = 443
    use_ssl        = true
    validate_ssl   = true
    request_method = "GET"
  }

  selected_regions = [
    "EUROPE",
    "USA",
  ]
}

resource "google_monitoring_alert_policy" "uptime" {
  count = var.enable_uptime_checks ? 1 : 0

  project               = var.project_id
  display_name          = "Dragon Shift Production Uptime"
  combiner              = "OR"
  enabled               = true
  notification_channels = [local.notification_channel_id]

  conditions {
    display_name = "Live endpoint uptime failure"

    condition_threshold {
      filter          = format("metric.type=\"monitoring.googleapis.com/uptime_check/check_passed\" AND resource.type=\"uptime_url\" AND metric.labels.check_id=\"%s\"", google_monitoring_uptime_check_config.live[0].uptime_check_id)
      duration        = "120s"
      comparison      = "COMPARISON_LT"
      threshold_value = 1

      aggregations {
        alignment_period   = "120s"
        per_series_aligner = "ALIGN_NEXT_OLDER"
      }

      trigger {
        count = 1
      }
    }
  }

  conditions {
    display_name = "Ready endpoint uptime failure"

    condition_threshold {
      filter          = format("metric.type=\"monitoring.googleapis.com/uptime_check/check_passed\" AND resource.type=\"uptime_url\" AND metric.labels.check_id=\"%s\"", google_monitoring_uptime_check_config.ready[0].uptime_check_id)
      duration        = "120s"
      comparison      = "COMPARISON_LT"
      threshold_value = 1

      aggregations {
        alignment_period   = "120s"
        per_series_aligner = "ALIGN_NEXT_OLDER"
      }

      trigger {
        count = 1
      }
    }
  }

  user_labels = local.common_labels

  documentation {
    content   = "Investigate GKE ingress, Cloud Armor, app readiness, and Cloud SQL health before reopening traffic."
    mime_type = "text/markdown"
  }
}
