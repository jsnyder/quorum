# mini-terraform

A tiny Terraform fixture used by quorum's context feature tests. It
contains a single `networking` module that provisions an AWS VPC with a
configurable CIDR block and name tag.

## Usage

Reference the module from a root configuration and pass a `name` plus
optional `cidr_block` variable. The module outputs the created VPC's ID
for downstream wiring.
