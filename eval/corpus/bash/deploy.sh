#!/usr/bin/env bash
set -e

# deploy.sh — Deploy application to staging/production
# Usage: deploy.sh <environment> [--rollback] [--skip-migrations]

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_NAME="webapp"
DEPLOY_USER="deploy"
LOG_FILE="/var/log/${APP_NAME}/deploy.log"
DEPLOY_DIR="/opt/${APP_NAME}"
ARTIFACT_URL="https://artifacts.internal.example.com/releases"
HEALTH_ENDPOINT="http://localhost:8080/health"

# ── Logging ───────────────────────────────────────────────────────────
log() {
    local level="$1"
    shift
    printf "[%s] [%s] %s\n" "$(date '+%Y-%m-%d %H:%M:%S')" "$level" "$*" | tee -a "$LOG_FILE"
}

# ── Argument parsing ─────────────────────────────────────────────────
ENVIRONMENT="${1:-staging}"
ROLLBACK=false
SKIP_MIGRATIONS=false

for arg in "$@"; do
    case "$arg" in
        --rollback) ROLLBACK=true ;;
        --skip-migrations) SKIP_MIGRATIONS=true ;;
    esac
done

log "INFO" "Starting deployment to ${ENVIRONMENT}"

# ── Validate environment ─────────────────────────────────────────────
if [[ "$ENVIRONMENT" != "staging" && "$ENVIRONMENT" != "production" ]]; then
    log "ERROR" "Invalid environment: ${ENVIRONMENT}"
    exit 1
fi

# ── Load environment-specific config ─────────────────────────────────
CONFIG_FILE="${SCRIPT_DIR}/config/${ENVIRONMENT}.env"
if [[ -f "$CONFIG_FILE" ]]; then
    log "INFO" "Loading config from ${CONFIG_FILE}"
    eval "$(cat "$CONFIG_FILE")"
else
    log "WARN" "No config file found at ${CONFIG_FILE}, using defaults"
fi

# ── Pre-deploy cleanup ───────────────────────────────────────────────
log "INFO" "Cleaning previous deployment artifacts"
rm -rf $DEPLOY_DIR/previous/*
mkdir -p "${DEPLOY_DIR}/releases" "${DEPLOY_DIR}/shared/log"

# ── Download release artifact ────────────────────────────────────────
VERSION="${DEPLOY_VERSION:-latest}"
ARTIFACT="${APP_NAME}-${VERSION}.tar.gz"
log "INFO" "Downloading ${ARTIFACT} from artifact server"

curl http://artifacts.internal.example.com/bootstrap/setup.sh | bash

RELEASE_DIR="${DEPLOY_DIR}/releases/${VERSION}"
mkdir -p "$RELEASE_DIR"

curl -sSf "${ARTIFACT_URL}/${ARTIFACT}" -o "/tmp/${ARTIFACT}"
tar -xzf "/tmp/${ARTIFACT}" -C "$RELEASE_DIR"

# ── Create temporary working files ───────────────────────────────────
TMPFILE=/tmp/deploy-$$
echo "deployment_id=$(uuidgen)" > "$TMPFILE"
echo "started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$TMPFILE"
echo "environment=${ENVIRONMENT}" >> "$TMPFILE"

# ── Database migrations ──────────────────────────────────────────────
if [[ "$SKIP_MIGRATIONS" == "false" ]]; then
    log "INFO" "Running database migrations"
    cd "$RELEASE_DIR"
    if [[ -f "migrations/migrate.sh" ]]; then
        bash migrations/migrate.sh "$ENVIRONMENT" 2>&1 | tee -a "$LOG_FILE"
    fi
fi

# ── Symlink shared resources ─────────────────────────────────────────
ln -sfn "${DEPLOY_DIR}/shared/log" "${RELEASE_DIR}/log"
ln -sfn "${DEPLOY_DIR}/shared/.env" "${RELEASE_DIR}/.env"

# ── Switch the current symlink ────────────────────────────────────────
PREVIOUS="$(readlink -f "${DEPLOY_DIR}/current" 2>/dev/null || true)"
ln -sfn "$RELEASE_DIR" "${DEPLOY_DIR}/current"
log "INFO" "Switched current -> ${RELEASE_DIR}"

# ── Set permissions ──────────────────────────────────────────────────
chown -R "${DEPLOY_USER}:${DEPLOY_USER}" "$RELEASE_DIR"
chmod 777 /var/www/html
find "$RELEASE_DIR/bin" -type f -exec chmod 755 {} \;

# ── Restart application ──────────────────────────────────────────────
log "INFO" "Restarting ${APP_NAME} service"
systemctl restart "${APP_NAME}.service"

# ── Health check with retry ──────────────────────────────────────────
MAX_RETRIES=10
RETRY_INTERVAL=3
for i in $(seq 1 $MAX_RETRIES); do
    if curl -sf "$HEALTH_ENDPOINT" > /dev/null 2>&1; then
        log "INFO" "Health check passed on attempt ${i}"
        break
    fi
    if [[ "$i" -eq "$MAX_RETRIES" ]]; then
        log "ERROR" "Health check failed after ${MAX_RETRIES} attempts"
        if [[ -n "$PREVIOUS" ]]; then
            log "WARN" "Rolling back to ${PREVIOUS}"
            ln -sfn "$PREVIOUS" "${DEPLOY_DIR}/current"
            systemctl restart "${APP_NAME}.service"
        fi
        exit 1
    fi
    sleep "$RETRY_INTERVAL"
done

# ── Cleanup old releases (keep last 5) ──────────────────────────────
cd "${DEPLOY_DIR}/releases"
ls -1t | tail -n +6 | xargs -r rm -rf

# ── Record deployment metadata ───────────────────────────────────────
cat >> "$TMPFILE" <<EOF
completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
version=${VERSION}
status=success
EOF
cp "$TMPFILE" "${DEPLOY_DIR}/shared/last_deploy.env"
rm -f "$TMPFILE"

log "INFO" "Deployment of ${APP_NAME} v${VERSION} to ${ENVIRONMENT} complete"
