#!/usr/bin/env bash
# hejiu45-oss — Linux one-command installer.
#
#   curl -fsSL https://raw.githubusercontent.com/Hejiu-Web/hejiu45-oss/main/install-linux.sh | sudo bash
#
# What it does:
#   1. detects the architecture (x86_64 / aarch64)
#   2. downloads the matching static binary from the latest GitHub release
#   3. verifies its SHA-256 against the published checksum file
#   4. installs to /usr/local/bin, creates a system user and data directory
#   5. generates the master key once, with 0600 permissions
#   6. writes a hardened systemd unit and starts the service
#   7. optionally provisions an admin password (printed once)
#
# Environment overrides:
#   HEJIU45_OSS_VERSION   release tag to install (default: latest)
#   HEJIU45_OSS_REPO      owner/repo (default: Hejiu-Web/hejiu45-oss)
#   HEJIU45_OSS_BIN_URL   install from an explicit binary URL instead
#   HEJIU45_OSS_LISTEN    listen address (default: 127.0.0.1:9000)
#   HEJIU45_OSS_TLS_MODE  off | manual | reverse-proxy (default: reverse-proxy)
#   HEJIU45_OSS_TLS_CERT  certificate path for --tls manual
#   HEJIU45_OSS_TLS_KEY   key path for --tls manual
#   HEJIU45_OSS_DOMAIN    public URL, e.g. https://oss.example.com
#   HEJIU45_OSS_ADMIN_PASSWORD  initial admin password (else generated)
#   HEJIU45_OSS_NGINX=1   also install a reverse-proxy Nginx site
#   HEJIU45_OSS_UNINSTALL=1  remove the service, binary and unit (keeps data)
#
# Tested on Debian 12, Ubuntu 22.04/24.04, Rocky/Alma 9.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
APP="hejiu45-oss"
BIN="/usr/local/bin/${APP}"
DATA_DIR="/var/lib/${APP}"
CONF_DIR="/etc/${APP}"
INSTALL_DIR="/opt/${APP}"
SERVICE="/etc/systemd/system/${APP}.service"
SERVICE_USER="${APP}"
REPO="${HEJIU45_OSS_REPO:-Hejiu-Web/hejiu45-oss}"
VERSION="${HEJIU45_OSS_VERSION:-latest}"
LISTEN="${HEJIU45_OSS_LISTEN:-127.0.0.1:9000}"
TLS_MODE="${HEJIU45_OSS_TLS_MODE:-reverse-proxy}"
TLS_CERT="${HEJIU45_OSS_TLS_CERT:-}"
TLS_KEY="${HEJIU45_OSS_TLS_KEY:-}"
DOMAIN="${HEJIU45_OSS_DOMAIN:-}"
ADMIN_PASSWORD="${HEJIU45_OSS_ADMIN_PASSWORD:-}"

if [[ -t 1 ]]; then
  BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'
  YELLOW=$'\033[33m'; CYAN=$'\033[36m'; RESET=$'\033[0m'
else
  BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; CYAN=""; RESET=""
fi

step() { printf '%s==>%s %s\n' "$CYAN" "$RESET" "$*"; }
ok()   { printf '%s  ok%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '%swarn%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die()  { printf '%serr %s%s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

require_root() {
  [[ "${EUID}" -eq 0 ]] || die "run as root (sudo bash install-linux.sh)"
}

detect_platform() {
  local os arch
  os="$(uname -s)"
  [[ "$os" == "Linux" ]] || die "this script is for Linux; use install-windows.ps1 on Windows"
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64)  ARCH="amd64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *) die "unsupported architecture: $arch (build from source: cargo build --release)" ;;
  esac
  ok "platform linux/${ARCH}"
}

download_binary() {
  local url tmp
  command -v curl >/dev/null 2>&1 || die "curl is required"
  if [[ -n "${HEJIU45_OSS_BIN_URL:-}" ]]; then
    url="${HEJIU45_OSS_BIN_URL}"
  elif [[ "$VERSION" == "latest" ]]; then
    url="https://github.com/${REPO}/releases/latest/download/${APP}-linux-${ARCH}"
  else
    url="https://github.com/${REPO}/releases/download/${VERSION}/${APP}-linux-${ARCH}"
  fi
  tmp="$(mktemp)"
  step "downloading ${url}"
  curl -fsSL --retry 3 --retry-delay 2 -o "$tmp" "$url" || die "download failed"

  # Verify against the published checksum when available. A missing checksum
  # file is fatal unless the operator opted out explicitly, because an
  # unverified download of a server binary is not something to do silently.
  local sums_url sums
  if [[ -z "${HEJIU45_OSS_BIN_URL:-}" ]]; then
    sums_url="${url%/*}/checksums-${APP}.txt"
    if curl -fsSL --retry 2 -o "${tmp}.sums" "$sums_url" 2>/dev/null; then
      sums="$(awk -v f="${APP}-linux-${ARCH}" '$2 == f || $2 == "*"f {print $1}' "${tmp}.sums")"
      [[ -n "$sums" ]] || die "could not find a checksum for ${APP}-linux-${ARCH}"
      echo "${sums}  ${tmp}" | sha256sum -c --quiet - || die "checksum verification FAILED"
      ok "sha256 verified"
      rm -f "${tmp}.sums"
    elif [[ "${HEJIU45_OSS_ALLOW_UNVERIFIED:-0}" == "1" ]]; then
      warn "no checksum file published; continuing because HEJIU45_OSS_ALLOW_UNVERIFIED=1"
    else
      rm -f "$tmp"
      die "no checksum file at ${sums_url}; refusing to install an unverified binary (set HEJIU45_OSS_ALLOW_UNVERIFIED=1 to override)"
    fi
  fi

  install -m 0755 "$tmp" "$BIN"
  rm -f "$tmp"
  ok "installed $("$BIN" --version 2>/dev/null || echo "$APP") to $BIN"
}

create_user_and_dirs() {
  step "preparing user and directories"
  if ! id -u "$SERVICE_USER" >/dev/null 2>&1; then
    useradd --system --home-dir "$DATA_DIR" --shell /usr/sbin/nologin "$SERVICE_USER"
    ok "created system user $SERVICE_USER"
  else
    ok "system user $SERVICE_USER exists"
  fi
  install -d -m 0750 -o root -g "$SERVICE_USER" "$CONF_DIR"
  install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$DATA_DIR"
  install -d -m 0750 -o root -g "$SERVICE_USER" "$INSTALL_DIR"
  chown -R "$SERVICE_USER":"$SERVICE_USER" "$DATA_DIR"
}

write_env_file() {
  step "writing $CONF_DIR/${APP}.env"
  if [[ -f "$CONF_DIR/${APP}.env" ]]; then
    warn "existing environment file kept; edit it to change settings"
    return
  fi
  local master
  # Generated once, never overwritten: losing it makes stored S3 secrets
  # unreadable (they are sealed with a key derived from it).
  master="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  umask 077
  cat > "$CONF_DIR/${APP}.env" <<EOF
# hejiu45-oss environment — read by systemd (EnvironmentFile).
# Contains secrets: keep mode 0600 and owner root:${SERVICE_USER}.
#
# Master key for envelope encryption of S3 secret keys. BACK THIS UP: without
# it, stored access keys cannot be decrypted. Rotating it requires re-issuing
# every access key.
HEJIU45_OSS_MASTER_KEY=${master}
EOF
  if [[ -n "$ADMIN_PASSWORD" ]]; then
    printf 'HEJIU45_OSS_ADMIN_PASSWORD=%s\n' "$ADMIN_PASSWORD" >> "$CONF_DIR/${APP}.env"
  fi
  chown root:"$SERVICE_USER" "$CONF_DIR/${APP}.env"
  chmod 0640 "$CONF_DIR/${APP}.env"
  ok "master key generated"
}

build_exec_args() {
  EXEC_ARGS="--data-dir ${DATA_DIR} --listen ${LISTEN} --tls ${TLS_MODE}"
  [[ -n "$DOMAIN" ]] && EXEC_ARGS+=" --public-url ${DOMAIN}"
  if [[ "$TLS_MODE" == "manual" ]]; then
    [[ -n "$TLS_CERT" && -n "$TLS_KEY" ]] || die "--tls manual needs HEJIU45_OSS_TLS_CERT and HEJIU45_OSS_TLS_KEY"
    EXEC_ARGS+=" --tls-cert ${TLS_CERT} --tls-key ${TLS_KEY}"
  fi
}

write_systemd_unit() {
  step "writing $SERVICE"
  build_exec_args
  cat > "$SERVICE" <<EOF
[Unit]
Description=hejiu45-oss — S3-compatible object storage
Documentation=https://github.com/${REPO}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${SERVICE_USER}
Group=${SERVICE_USER}
EnvironmentFile=${CONF_DIR}/${APP}.env
ExecStart=${BIN} ${EXEC_ARGS}
Restart=on-failure
RestartSec=3
# Give in-flight uploads a moment to finish.
TimeoutStopSec=30

# --- hardening -------------------------------------------------------------
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectSystem=strict
ProtectHome=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
RestrictNamespaces=yes
LockPersonality=yes
# Only the data directory is writable.
ReadWritePaths=${DATA_DIR}
# Network: it is a server, so it needs sockets (but no raw sockets).
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
SystemCallFilter=@system-service
SystemCallErrorNumber=EPERM

LimitNOFILE=65536
MemoryMax=

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable "$APP" >/dev/null 2>&1 || true
  ok "unit installed"
}

start_service() {
  step "starting ${APP}"
  systemctl restart "$APP"
  local i
  for i in $(seq 1 30); do
    if systemctl is-active --quiet "$APP"; then
      ok "service is running"
      return
    fi
    sleep 0.5
  done
  systemctl status "$APP" --no-pager || true
  journalctl -u "$APP" -n 40 --no-pager || true
  die "service failed to start (see the log above)"
}

install_nginx_site() {
  [[ "${HEJIU45_OSS_NGINX:-0}" == "1" ]] || return 0
  command -v nginx >/dev/null 2>&1 || { warn "nginx not installed; skipping site config"; return 0; }
  [[ -n "$DOMAIN" ]] || die "HEJIU45_OSS_NGINX=1 needs HEJIU45_OSS_DOMAIN (e.g. https://oss.example.com)"
  local host domain_only dir
  domain_only="${DOMAIN#https://}"; domain_only="${domain_only#http://}"; domain_only="${domain_only%%/*}"
  step "installing Nginx site for ${domain_only}"
  if [[ -d /etc/nginx/sites-available ]]; then
    dir="/etc/nginx/sites-available"
    CONFIG_TARGET="/etc/nginx/sites-available/${APP}.conf"
  else
    dir="/etc/nginx/conf.d"
    CONFIG_TARGET="/etc/nginx/conf.d/${APP}.conf"
  fi
  # Render the shipped template with the real hostname and upstream.
  cat > "$CONFIG_TARGET" <<NGINX
# Generated by install-linux.sh — see deploy/nginx/hejiu45-oss.conf upstream.
upstream hejiu45_oss { server $(echo "$LISTEN" | tr -d ' ') ; keepalive 64; }

server {
    listen 80;
    listen [::]:80;
    server_name ${domain_only};
    location /.well-known/acme-challenge/ { root /var/www/certbot; }
    location / { return 301 https://\$host\$request_uri; }
}

server {
    listen 443 ssl;
    listen [::]:443 ssl;
    http2 on;
    server_name ${domain_only};

    ssl_certificate     /etc/letsencrypt/live/${domain_only}/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/${domain_only}/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_session_cache shared:HJ45_SSL:10m;

    add_header Strict-Transport-Security "max-age=31536000; includeSubDomains" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header X-Frame-Options "DENY" always;

    client_max_body_size 1g;
    proxy_request_buffering off;
    proxy_buffering off;
    proxy_read_timeout 300s;
    proxy_send_timeout 300s;

    proxy_http_version 1.1;
    proxy_set_header Host              \$host;
    proxy_set_header X-Real-IP         \$remote_addr;
    proxy_set_header X-Forwarded-For   \$proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto \$scheme;
    proxy_set_header Connection        "";

    location / { proxy_pass http://hejiu45_oss; }
    location /assets/ { proxy_pass http://hejiu45_oss; expires 1y; add_header Cache-Control "public, immutable"; }
    location = /healthz { proxy_pass http://hejiu45_oss; access_log off; }
}
NGINX
  if [[ -d /etc/nginx/sites-enabled && "$dir" == "/etc/nginx/sites-available" ]]; then
    ln -sfn "$CONFIG_TARGET" "/etc/nginx/sites-enabled/${APP}.conf"
  fi
  if nginx -t 2>/dev/null; then
    systemctl reload nginx 2>/dev/null || true
    ok "nginx configured for ${domain_only}"
    if command -v certbot >/dev/null 2>&1; then
      warn "issue the certificate with: certbot --nginx -d ${domain_only}"
    else
      warn "install certbot, then run: certbot --nginx -d ${domain_only}"
    fi
  else
    warn "nginx -t failed; inspect $CONFIG_TARGET"
  fi
}

uninstall() {
  step "uninstalling ${APP}"
  systemctl stop "$APP" 2>/dev/null || true
  systemctl disable "$APP" 2>/dev/null || true
  rm -f "$SERVICE"
  systemctl daemon-reload
  rm -f "$BIN"
  rm -f "/etc/nginx/sites-enabled/${APP}.conf" "/etc/nginx/sites-available/${APP}.conf" "/etc/nginx/conf.d/${APP}.conf"
  ok "binary and service removed"
  printf '%sData kept%s at %s and %s (delete manually if you are sure).\n' "$BOLD" "$RESET" "$DATA_DIR" "$CONF_DIR"
}

print_summary() {
  local url="http://${LISTEN}"
  [[ -n "$DOMAIN" ]] && url="$DOMAIN"
  cat <<EOF

${BOLD}hejiu45-oss is installed.${RESET}

  Console    ${url}/
  S3 API     ${url}/s3   (path-style addressing)
  Config     ${CONF_DIR}/${APP}.env          (master key — back it up)
  Data       ${DATA_DIR}
  Service    systemctl status ${APP}
  Logs       journalctl -u ${APP} -f
  Backup     ${BIN} --data-dir ${DATA_DIR} --backup-to /root/hj45-\$(date +%F).redb

${DIM}First login:${RESET} the initial admin password was printed by the service on
first start. Retrieve it with:

  journalctl -u ${APP} -n 50 --no-pager | sed -n '/Initial admin/,/─/p'

${DIM}Next steps${RESET}
  * Put this server behind HTTPS: set HEJIU45_OSS_DOMAIN and use
    ${CYAN}--tls reverse-proxy${RESET} with the shipped Nginx/Caddy config, or
    point ${CYAN}--tls manual${RESET} at a certbot certificate.
  * Back up ${CYAN}${CONF_DIR}/${APP}.env${RESET} together with the data directory.
EOF
}

main() {
  require_root
  if [[ "${HEJIU45_OSS_UNINSTALL:-0}" == "1" ]]; then
    uninstall
    exit 0
  fi
  step "installing ${APP}"
  detect_platform
  download_binary
  create_user_and_dirs
  write_env_file
  write_systemd_unit
  start_service
  install_nginx_site
  print_summary
}

main "$@"
