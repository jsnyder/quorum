# Fixture: iam-wildcard-action

# match: wildcard action inside aws_iam_policy_document
data "aws_iam_policy_document" "too_broad" {
  statement {
    actions   = ["*"]
    resources = ["*"]
  }
}

# no-match: explicit action allowlist
data "aws_iam_policy_document" "narrow" {
  statement {
    actions   = ["s3:GetObject", "s3:PutObject"]
    resources = ["arn:aws:s3:::my-bucket/*"]
  }
}
