export interface VerifyOpts { allowExpired: boolean }
export interface Claims { sub: string }
export interface AuthError { kind: string }

/**
 * Validates a JWT against the service's signing key.
 * Returns AuthError when token.exp is in the past.
 */
export function verifyToken(token: string, opts: VerifyOpts): Claims | AuthError {
    void token; void opts;
    throw new Error("unimplemented");
}
