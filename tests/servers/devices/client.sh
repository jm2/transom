#!/usr/bin/env bash
# Runs as root in the container (rootless podman: root only inside the user
# namespace): start CUPS with a PDF printer named TransomPDF, then run the given
# command (the spike) as root too, so libcups and the pty belong to the same user.
set -euo pipefail
mkdir -p /run/cups /out/pdf
chmod 1777 /out/pdf
# Every PDF into /out/pdf (mounted by the check), whoever printed it.
sed -i -e 's|^Out .*|Out /out/pdf|' -e 's|^#\?AnonDirName .*|AnonDirName /out/pdf|' /etc/cups/cups-pdf.conf
/usr/sbin/cupsd
for _ in $(seq 50); do lpstat -r >/dev/null 2>&1 && break; sleep 0.1; done
lpadmin -p TransomPDF -E -v cups-pdf:/ -m CUPS-PDF_noopt.ppd 2>/dev/null ||
    lpadmin -p TransomPDF -E -v cups-pdf:/ -P "$(ls /usr/share/cups/model/CUPS-PDF*.ppd | head -1)"
lpadmin -d TransomPDF
echo "printers: $(lpstat -p | awk '{print $2}' | tr '\n' ' ')"
# Serial: a pty pair; the session gets /dev/ttyTRANSOM (COM9 in the check) and a
# responder on the other end answers each line with "reply: <line>".
socat pty,raw,echo=0,link=/dev/ttyTRANSOM pty,raw,echo=0,link=/dev/ttyPEER &
for _ in $(seq 50); do [[ -e /dev/ttyPEER ]] && break; sleep 0.1; done
( while IFS= read -r line < /dev/ttyPEER; do
      echo "serial got: ${line%$'\r'}"; printf 'reply: %s\r\n' "${line%$'\r'}" > /dev/ttyPEER
  done ) &
exec "$@"
