# Generic confidential service module
# Creates a security group + ECS instance with optional TDX support.

resource "alicloud_security_group" "this" {
  security_group_name = "${var.project_name}-${var.service_name}-sg"
  vpc_id              = var.vpc_id
  description         = "Security group for ${var.service_name}"
}

locals {
  # trustiflux-api-server exposes CDH resource injection API on 8006.
  effective_sg_ports = distinct(concat(var.sg_ports, ["8006/8006"]))
}

resource "alicloud_security_group_rule" "ports" {
  for_each          = toset(local.effective_sg_ports)
  type              = "ingress"
  ip_protocol       = "tcp"
  port_range        = each.value
  security_group_id = alicloud_security_group.this.id
  cidr_ip           = var.security_group_allowed_cidr
}

resource "alicloud_security_group_rule" "ports_vpc" {
  for_each          = toset(local.effective_sg_ports)
  type              = "ingress"
  ip_protocol       = "tcp"
  port_range        = each.value
  security_group_id = alicloud_security_group.this.id
  cidr_ip           = var.vpc_cidr
}

# gn8v-tee（异构机密 GPU）规格已内置 TEE/TDX，勿再传 SecurityOptions.TDX
locals {
  apply_tdx_security_options = var.tdx && !strcontains(var.instance_type, "gn8v-tee")
}

resource "alicloud_instance" "this" {
  instance_name     = "${var.project_name}-${var.service_name}"
  availability_zone = var.zone_id
  security_groups   = [alicloud_security_group.this.id]
  instance_type     = var.instance_type
  image_id          = var.image_id
  vswitch_id        = var.vswitch_id
  private_ip        = var.private_ip != "" ? var.private_ip : null

  system_disk_category = "cloud_essd"
  system_disk_size     = var.disk_size

  internet_max_bandwidth_out = 10

  dynamic "security_options" {
    for_each = local.apply_tdx_security_options ? [1] : []
    content {
      confidential_computing_mode = "TDX"
    }
  }

  tags = {
    Name    = "${var.project_name}-${var.service_name}"
    Project = var.project_name
  }
}
