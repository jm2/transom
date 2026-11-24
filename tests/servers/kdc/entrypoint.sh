#!/usr/bin/env bash
# /run/transom/creds: line 1 user, line 2 password (throwaway). Writes the RDP
# service keytab to /out/termsrv.keytab and runs the KDC in the foreground.
set -euo pipefail
{ read -r user; read -r pass; } < /run/transom/creds
# Passwords go in on stdin, not argv: without -P / -pw both tools prompt for the
# password twice (MIT krb5 reads the answers from stdin, unbuffered, when it is not
# a terminal).
mkey=$(head -c 24 /dev/urandom | base64)
printf '%s\n%s\n' "$mkey" "$mkey" | kdb5_util create -s -r TRANSOM.TEST >/dev/null
unset mkey
printf '%s\n%s\n' "$pass" "$pass" | kadmin.local -q "addprinc $user" >/dev/null
kadmin.local -q "addprinc -randkey TERMSRV/localhost" >/dev/null
rm -f /out/termsrv.keytab
kadmin.local -q "ktadd -k /out/termsrv.keytab TERMSRV/localhost" >/dev/null
chmod 644 /out/termsrv.keytab
echo "KDC ready"
exec krb5kdc -n
