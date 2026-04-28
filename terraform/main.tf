# Main Terraform configuration for cai
#
# Services are defined via the `services` variable (populated from profile.json
# files by `make deploy PROFILE=xxx`). Each entry creates an OSS upload,
# image import, and ECS instance through the generic modules/service module.

# Random suffix for unique bucket name
resource "random_string" "bucket_suffix" {
  length  = 8
  special = false
  upper   = false
}

# OSS bucket for storing cai images
resource "alicloud_oss_bucket" "cai" {
  bucket = "${var.project_name}-images-${random_string.bucket_suffix.result}"

  tags = {
    Name    = "${var.project_name}-images"
    Project = var.project_name
  }
}

resource "alicloud_oss_bucket_acl" "cai" {
  bucket = alicloud_oss_bucket.cai.bucket
  acl    = "private"
}

# ---------------------------------------------------------------------------
# Image upload & import (driven by var.services declarations)
# ---------------------------------------------------------------------------
locals {
  image_dir = "${path.root}/../image/output"
}

resource "alicloud_oss_bucket_object" "service_images" {
  for_each = var.services
  bucket   = alicloud_oss_bucket.cai.bucket
  key      = "images/${each.value.image_file}"
  source   = "${local.image_dir}/${each.value.image_file}"

  lifecycle {
    ignore_changes = [source]
  }
}

resource "alicloud_image_import" "services" {
  for_each = var.services

  image_name   = replace(each.value.image_file, ".qcow2", "")
  description  = "CAI ${each.key} image (${var.image_type})"
  os_type      = "linux"
  platform     = "Aliyun"
  architecture = "x86_64"
  boot_mode    = "UEFI"

  disk_device_mapping {
    oss_bucket      = alicloud_oss_bucket.cai.bucket
    oss_object      = alicloud_oss_bucket_object.service_images[each.key].key
    disk_image_size = 30
  }

  features {
    nvme_support = "supported"
  }

  timeouts {
    create = "30m"
  }

  depends_on = [alicloud_oss_bucket_object.service_images]
}

# ---------------------------------------------------------------------------
# Networking
# ---------------------------------------------------------------------------
resource "alicloud_vpc" "cai" {
  vpc_name   = "${var.project_name}-vpc"
  cidr_block = var.vpc_cidr
}

resource "alicloud_vswitch" "cai" {
  vswitch_name = "${var.project_name}-vsw"
  vpc_id       = alicloud_vpc.cai.id
  cidr_block   = var.vswitch_cidr
  zone_id      = var.zone_id
}

# ---------------------------------------------------------------------------
# Trustee (attestation infrastructure — optional, SECRET_MODE=trustee only)
# ---------------------------------------------------------------------------
module "trustee" {
  count  = var.deploy_trustee ? 1 : 0
  source = "./modules/trustee"

  project_name                = var.project_name
  vpc_id                      = alicloud_vpc.cai.id
  vpc_cidr                    = var.vpc_cidr
  vswitch_id                  = alicloud_vswitch.cai.id
  zone_id                     = var.zone_id
  instance_type               = var.trustee_instance_type
  private_ip                  = var.trustee_private_ip
  security_group_allowed_cidr = var.security_group_allowed_cidr

  providers = {
    alicloud = alicloud
  }
}

# ---------------------------------------------------------------------------
# Service instances (generic, driven by var.services)
# ---------------------------------------------------------------------------
module "services" {
  for_each = var.services
  source   = "./modules/service"

  project_name                = var.project_name
  service_name                = each.key
  vpc_id                      = alicloud_vpc.cai.id
  vpc_cidr                    = var.vpc_cidr
  vswitch_id                  = alicloud_vswitch.cai.id
  zone_id                     = var.zone_id
  instance_type               = each.value.instance_type
  image_id                    = alicloud_image_import.services[each.key].id
  private_ip                  = each.value.ip
  tdx                         = each.value.tdx
  disk_size                   = each.value.disk_size
  sg_ports                    = each.value.sg_ports
  security_group_allowed_cidr = var.security_group_allowed_cidr

  providers = {
    alicloud = alicloud
  }
}
