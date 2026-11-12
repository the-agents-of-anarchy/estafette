# The owner's Mac stays awake while the lid is open: a macOS idle-sleep guard that is started, watched and stopped here.

set shell := ["bash", "-euo", "pipefail", "-c"]

guard_executable := "/usr/bin/caffeinate"
guard_lifetime_seconds := "315360000"
guard_command_line := guard_executable + " -i -m -t " + guard_lifetime_seconds
guard_pid_recipe := just_executable() + " --quiet awake::resolve-guard-pid"

[doc("Hold a macOS claim against idle sleep on battery and on power, detached from this shell; a closed lid still sleeps; a second call leaves the one guard")]
[group('guard')]
start:
    #!/usr/bin/env bash
    set -euo pipefail
    [[ "$(uname)" == Darwin ]] || { printf 'awake: nothing to do on %s\n' "$(uname)"; exit 0; }
    if pid="$({{ guard_pid_recipe }})"; then
        printf 'awake: a guard is already up on pid %s\n' "$pid"
        exit 0
    fi
    nohup {{ guard_command_line }} </dev/null >/dev/null 2>&1 &
    printf 'awake: guard started on pid %s\n' "$!"

[doc("Print `awake: on pid <pid> since <elapsed>` and exit 0 while the guard lives and macOS lists its claim, else `awake: off` and exit 1")]
[group('guard')]
[no-exit-message]
status:
    #!/usr/bin/env bash
    set -euo pipefail
    [[ "$(uname)" == Darwin ]] || { printf 'awake: nothing to do on %s\n' "$(uname)"; exit 0; }
    if pid="$({{ guard_pid_recipe }})" && pmset -g assertions | grep -q "pid $pid(caffeinate)"; then
        printf 'awake: on pid %s since %s\n' "$pid" "$(ps -o etime= -p "$pid" | tr -d ' ')"
        exit 0
    fi
    printf 'awake: off\n'
    exit 1

[doc("End the guard this module started, found by its exact command line, and no other caffeinate; none running is not an error")]
[group('guard')]
stop:
    #!/usr/bin/env bash
    set -euo pipefail
    [[ "$(uname)" == Darwin ]] || { printf 'awake: nothing to do on %s\n' "$(uname)"; exit 0; }
    if pid="$({{ guard_pid_recipe }})"; then
        kill "$pid"
        printf 'awake: guard on pid %s stopped\n' "$pid"
    else
        printf 'awake: no guard is up\n'
    fi

[doc("Print the pid of the caffeinate process whose command line is exactly the guard's, exit 1 when none")]
[private]
resolve-guard-pid:
    #!/usr/bin/env bash
    set -euo pipefail
    for pid in $(pgrep -x caffeinate || true); do
        if [[ "$(ps -o command= -p "$pid")" == {{ quote(guard_command_line) }} ]]; then
            printf '%s\n' "$pid"
            exit 0
        fi
    done
    exit 1
