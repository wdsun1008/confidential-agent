output "instance_id" {
  description = "ECS instance ID"
  value       = alicloud_instance.this.id
}

output "private_ip" {
  description = "Instance private IP"
  value       = alicloud_instance.this.private_ip
}

output "public_ip" {
  description = "Instance public IP"
  value       = alicloud_instance.this.public_ip
}
