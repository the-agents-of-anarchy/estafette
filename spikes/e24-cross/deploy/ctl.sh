#!/bin/sh
# E24 per-machine control. usage:
#   ctl.sh relay                      start iroh-relay (relay.toml) in the background
#   ctl.sh proc <logname> <env...> ./e24-cross <args>   start a program, stdout to logs/<logname>
#   ctl.sh kill <logname>...          SIGKILL the named programs and, with `relay`, the relay; prints the clock (ms) just before and after
#   ctl.sh stopall                    kill everything of E24 on this machine, empty logs/ and pids/
#   ctl.sh now                        print the clock (ms)
cd /root/e24 2>/dev/null || cd /home/lab/e24x 2>/dev/null || exit 1
mkdir -p logs pids
case "$1" in
  relay)
    nohup ./iroh-relay --config-path relay.toml > logs/relay.log 2>&1 &
    echo $! > pids/relay.pid
    ;;
  proc)
    shift
    log=$1
    shift
    nohup env "$@" > "logs/$log" 2>&1 &
    echo $! > "pids/$log.pid"
    ;;
  kill)
    shift
    date +%s%3N
    for n in "$@"; do
      if [ "$n" = relay ]; then p=pids/relay.pid; else p="pids/$n.pid"; fi
      [ -f "$p" ] && kill -9 "$(cat "$p")" 2>/dev/null
    done
    date +%s%3N
    ;;
  stopall)
    for p in pids/*.pid; do
      [ -f "$p" ] && kill -9 "$(cat "$p")" 2>/dev/null
    done
    sleep 0.5
    rm -f pids/*.pid logs/e24-*.log logs/m-*.log logs/h-*.log logs/relay.log
    ;;
  now)
    date +%s%3N
    ;;
esac
