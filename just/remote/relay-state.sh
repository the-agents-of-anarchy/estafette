#!/usr/bin/env bash
set -uo pipefail

readonly RELAY_HTTPS_PORT=443

host_letter="$1"
host_unit="$2"
certificate_cache="$3"

relay_port_listening() {
    ss -H -t -l "sport = :$RELAY_HTTPS_PORT" | /usr/bin/grep -q .
}

printf 'host %s: unit %s is %s\n' "$host_letter" "$host_unit" "$(systemctl is-active "$host_unit")"
if relay_port_listening; then
    printf 'host %s: TCP %s listens\n' "$host_letter" "$RELAY_HTTPS_PORT"
else
    printf 'host %s: TCP %s does not listen\n' "$host_letter" "$RELAY_HTTPS_PORT"
fi
if ! test -d "$certificate_cache"; then
    printf 'host %s: the relay certificate cache directory does not exist\n' "$host_letter"
    exit 0
fi
printf 'host %s: relay certificate cache files: %s\n' "$host_letter" "$(find "$certificate_cache" -type f | wc -l)"
TZ=UTC find "$certificate_cache" -type f -printf "host $host_letter: cache file %f modified %TY-%Tm-%Td %TT UTC\n"
