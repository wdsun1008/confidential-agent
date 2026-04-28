variable "project_name" {
  type = string
}

variable "service_name" {
  type = string
}

variable "vpc_id" {
  type = string
}

variable "vpc_cidr" {
  type = string
}

variable "vswitch_id" {
  type = string
}

variable "zone_id" {
  type = string
}

variable "instance_type" {
  type    = string
  default = "ecs.g8i.xlarge"
}

variable "image_id" {
  type    = string
  default = null
}


variable "private_ip" {
  type    = string
  default = ""
}

variable "tdx" {
  type    = bool
  default = true
}

variable "disk_size" {
  type    = number
  default = 200
}

variable "sg_ports" {
  type    = list(string)
  default = ["22/22"]
}

variable "security_group_allowed_cidr" {
  type    = string
  default = "0.0.0.0/0"
}
