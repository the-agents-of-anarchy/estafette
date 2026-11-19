#!/usr/bin/env bash
set -euo pipefail

readonly POLL_SECONDS=5
readonly NAME_STAND_IN="<relay name>"
readonly ADDRESS_STAND_IN="<address>"
readonly DOTTED_ADDRESS='[0-9]{1,3}(\.[0-9]{1,3}){3}'

host_letter="$1"
relay_name="$2"
bound_seconds="$3"

handshake_failure() {
    CURL_SSL_BACKEND=secure-transport /usr/bin/curl -sS -o /dev/null --max-time "$1" "https://$relay_name/" 2>&1 | head -n 1
    return "${PIPESTATUS[0]}"
}

without_name_or_address() {
    printf '%s\n' "${1//"$relay_name"/$NAME_STAND_IN}" | sed -E "s/$DOTTED_ADDRESS/$ADDRESS_STAND_IN/g"
}

last_failure="no handshake was tried"
deadline=$((SECONDS + bound_seconds))
while remaining=$((deadline - SECONDS)); [ "$remaining" -gt 0 ]; do
    if last_failure="$(handshake_failure "$remaining")"; then
        printf 'host %s: relay answers TLS by name with a chain the Mac trusts\n' "$host_letter"
        exit 0
    fi
    sleep "$POLL_SECONDS"
done
printf 'host %s: relay did not answer TLS by name within %s s; last handshake: %s\n' "$host_letter" "$bound_seconds" "$(without_name_or_address "$last_failure")"
exit 1
