# SPDX-License-Identifier: GPL-3.0-or-later
# Sourced by the test scripts (from the repository root): sets STATE, the directory for
# test credentials, logs and screenshots, ${TMPDIR:-/var/tmp}/transom-test-UID. It is
# created 0700, and the scripts refuse to go on unless it is a directory owned by this
# user and not a symlink: /var/tmp is shared, and a directory (or symlink) someone else
# put there first would let them read or swap the test passwords.

# owned_dir DIR: DIR is a directory owned by this user, and not a symlink.
owned_dir() { [[ -d $1 && ! -L $1 && -O $1 ]]; }

STATE="${TMPDIR:-/var/tmp}/transom-test-$(id -u)"
# Fails harmlessly when something is already there; the check below decides.
mkdir -p -m 700 "$STATE" 2>/dev/null || true
owned_dir "$STATE" || {
    echo "$STATE is not a directory owned by $(id -un), or is a symlink: refusing to use it" >&2
    exit 2
}
# Ours, but it may predate the 0700 rule.
chmod 700 "$STATE"
