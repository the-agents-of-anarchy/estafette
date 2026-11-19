#!/usr/bin/env bash
set -euo pipefail

readonly TOPIC_JOINED_EVENT="gossip.topic_joined"
readonly NEIGHBOR_UP_EVENT="gossip.neighbor_up"
readonly EVENT_KEY='"event":'
readonly POLL_SECONDS=2

host_letter="$1"
bound_seconds="$2"
events_file="$3"

lines_holding() {
    /usr/bin/grep -a -c -F -- "$1" "$events_file" || test "$?" = 1
}

lines_of_event() {
    lines_holding "$EVENT_KEY\"$1\""
}

event_counts() {
    printf '%s lines %s, %s lines %s, events lines %s' \
        "$TOPIC_JOINED_EVENT" "$(lines_of_event "$TOPIC_JOINED_EVENT")" \
        "$NEIGHBOR_UP_EVENT" "$(lines_of_event "$NEIGHBOR_UP_EVENT")" \
        "$(lines_holding "$EVENT_KEY")"
}

stands_joined() {
    [ "$(lines_of_event "$TOPIC_JOINED_EVENT")" -gt 0 ] && [ "$(lines_of_event "$NEIGHBOR_UP_EVENT")" -gt 0 ]
}

if ! test -r "$events_file"; then
    printf 'host %s: no events file is held open by a running host process\n' "$host_letter"
    exit 1
fi
deadline=$((SECONDS + bound_seconds))
until stands_joined; do
    if [ "$SECONDS" -ge "$deadline" ]; then
        printf 'host %s did not stand joined within %s s: %s\n' "$host_letter" "$bound_seconds" "$(event_counts)"
        exit 1
    fi
    sleep "$POLL_SECONDS"
done
printf 'host %s stands joined: %s\n' "$host_letter" "$(event_counts)"
