terraform {
  required_version = ">= 1.0"
}

resource "aws_vpc" "this" {
  cidr_block = var.cidr_block
  tags       = { Name = var.name }
}
