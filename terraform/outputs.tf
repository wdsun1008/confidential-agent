output "vpc_id" {
  description = "VPC ID"
  value       = alicloud_vpc.cai.id
}

output "oss_bucket" {
  description = "OSS bucket for cai images"
  value       = alicloud_oss_bucket.cai.bucket
}

# Trustee outputs (only when deploy_trustee = true)
output "trustee_private_ip" {
  description = "Trustee instance private IP"
  value       = var.deploy_trustee ? module.trustee[0].private_ip : null
}

output "trustee_public_ip" {
  description = "Trustee instance public IP"
  value       = var.deploy_trustee ? module.trustee[0].public_ip : null
}

output "trustee_private_url" {
  description = "Trustee service private URL"
  value       = var.deploy_trustee ? module.trustee[0].private_url : null
}

output "trustee_public_url" {
  description = "Trustee service public URL"
  value       = var.deploy_trustee ? module.trustee[0].public_url : null
}

# Generic service outputs
output "service_private_ips" {
  description = "Private IPs of all deployed services"
  value       = { for name, svc in module.services : name => svc.private_ip }
}

output "service_public_ips" {
  description = "Public IPs of all deployed services"
  value       = { for name, svc in module.services : name => svc.public_ip }
}

output "service_instance_ids" {
  description = "Instance IDs of all deployed services"
  value       = { for name, svc in module.services : name => svc.instance_id }
}
