#!/usr/bin/env bash
# Sets the throwaway password from /run/transom/creds (user, password), makes a
# fresh TLS cert and runs xrdp-sesman + xrdp in the foreground.
set -euo pipefail
{ read -r _user; read -r pass; } < /run/transom/creds
echo "rdp:$pass" | chpasswd
openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=transom-xrdp-test" \
    -keyout /etc/xrdp/key.pem -out /etc/xrdp/cert.pem 2>/dev/null
chmod 640 /etc/xrdp/key.pem
echo "TLS fingerprint: $(openssl x509 -in /etc/xrdp/cert.pem -noout -fingerprint -sha256 | cut -d= -f2)"
/usr/sbin/xrdp-sesman --nodaemon &
exec /usr/sbin/xrdp --nodaemon
