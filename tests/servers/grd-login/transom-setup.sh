#!/usr/bin/env bash
# /run/transom/creds: line 1 RDP user, 2 RDP password (grd's system credentials),
# line 3 login user, 4 login password (the GNOME account). Throwaway values.
set -euo pipefail
{ read -r rdp_user; read -r rdp_pass; read -r login_user; read -r login_pass; } < /run/transom/creds
id "$login_user" >/dev/null 2>&1 || useradd -m "$login_user"
echo "$login_user:$login_pass" | chpasswd
d=/var/lib/gnome-remote-desktop/.local/share/gnome-remote-desktop
mkdir -p "$d"
if [[ ! -f $d/tls.crt ]]; then
    openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=transom-grd-login-test" \
        -keyout "$d/tls.key" -out "$d/tls.crt" 2>/dev/null
fi
chown -R gnome-remote-desktop: /var/lib/gnome-remote-desktop
grdctl --system rdp set-tls-cert "$d/tls.crt"
grdctl --system rdp set-tls-key "$d/tls.key"
# The password goes in on stdin, not argv: grdctl reads it there when it isn't given.
printf '%s\n' "$rdp_pass" | grdctl --system rdp set-credentials "$rdp_user"
grdctl --system rdp enable
echo "TLS fingerprint: $(openssl x509 -in "$d/tls.crt" -noout -fingerprint -sha256 | cut -d= -f2)" > /run/transom/fingerprint
