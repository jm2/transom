#!/usr/bin/env bash
# /run/transom/creds: line 1 user, line 2 password (throwaway). Writes the RDP
# service keytab to /out/termsrv.keytab and runs the KDC in the foreground.
set -euo pipefail
{ read -r user; read -r pass; } < /run/transom/creds
kdb5_util create -s -r TRANSOM.TEST -P "$(head -c 24 /dev/urandom | base64)" >/dev/null
kadmin.local -q "addprinc -pw $pass $user" >/dev/null
kadmin.local -q "addprinc -randkey TERMSRV/localhost" >/dev/null
rm -f /out/termsrv.keytab
kadmin.local -q "ktadd -k /out/termsrv.keytab TERMSRV/localhost" >/dev/null
chmod 644 /out/termsrv.keytab
echo "KDC ready"
exec krb5kdc -n
