# Fixture: iam-wildcard-resource

# match: resources wildcard
data "aws_iam_policy_document" "star_resource" {
  statement {
    actions   = ["s3:GetObject"]
    resources = ["*"]
  }
}

# no-match: specific ARN
data "aws_iam_policy_document" "scoped" {
  statement {
    actions   = ["s3:GetObject"]
    resources = ["arn:aws:s3:::my-bucket/*"]
  }
}
