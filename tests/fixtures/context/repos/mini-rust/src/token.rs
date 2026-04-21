pub struct VerifyOpts { pub allow_expired: bool }
pub struct Claims { pub sub: String }
pub struct AuthError;

/// Validates a JWT against the service's signing key.
/// Returns AuthError::Expired if the token's `exp` is in the past.
pub fn verify_token(token: &str, opts: VerifyOpts) -> Result<Claims, AuthError> {
    let _ = (token, opts);
    unimplemented!()
}
