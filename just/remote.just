# Lab server on Hetzner: terminal, shell, and the backstop check.
# Every recipe looks the server's address up by name from the live API at run
# time, so a replaced server or a new address needs no edit here.
# Another server: LAB_SERVER=<name> just remote::<recipe>.

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := '..'

server := env("LAB_SERVER", "estafette-lab")
key := env("LAB_KEY", home_directory() / ".ssh" / "estafette_lab")
known_hosts := home_directory() / ".ssh" / "known_hosts_estafette_lab"
ssh_opts := "-i " + key + " -o IdentitiesOnly=yes -o UserKnownHostsFile=" + known_hosts + " -o StrictHostKeyChecking=accept-new"
lab_root := env("LAB_ROOT", "/tmp/estafette-lab")
lab_binary := env("LAB_BINARY", "target/debug/estafette")
lab_network := "lab"
lab_directories := "XDG_CONFIG_HOME=" + lab_root / "config" + " XDG_DATA_HOME=" + lab_root / "data" + " XDG_RUNTIME_DIR=" + lab_root / "run"
lab_server_type := env("LAB_SERVER_TYPE", "cpx22")
lab_location := "nbg1"
lab_image := "ubuntu-24.04"
lab_resource_name := "estafette-lab"
lab_skill := ".claude/skills/run-hetzner-lab"
lab_owner_label := "owner=estafette"
lab_setup_wait_tries := "120"
lab_setup_poll_seconds := "10"
remote_tree := "/home/lab/work/estafette"
remote_binary := remote_tree / "target/release/estafette"
remote_host_root := "/root/estafette-lab"
remote_host_directories := "XDG_CONFIG_HOME=" + remote_host_root / "config" + " XDG_DATA_HOME=" + remote_host_root / "data" + " XDG_RUNTIME_DIR=" + remote_host_root / "run"
host_unit := "estafette-host"
member_unit_prefix := "estafette-member-"
lab_config_file := lab_root / "config/estafette/networks" / lab_network + ".toml"
remote_host_config_file := remote_host_root / "config/estafette/networks" / lab_network + ".toml"
remote_host_certificate_file := remote_host_root / "data/estafette/infra" / lab_network + ".cert"
remote_host_certificate_temporary := remote_host_certificate_file + ".tmp"
remote_host_line_file := remote_host_root / "host-line"
host_record_under_root := "docs/orchestration_log/recon/2026-10-24/lab/host.txt"
root_checkout_command := 'dirname "$(git rev-parse --path-format=absolute --git-common-dir)"'

[doc("List this module's recipes (what bare `just remote` runs)")]
[group('read')]
help:
    @just --list remote

[doc("Print the server's current IPv4 address, from the live API")]
[group('read')]
ip:
    @hcloud server ip {{ server }}

[doc("Open a mosh terminal on the server; extra words run there, e.g. tmux attach -t lab")]
[group('act')]
mosh user="lab" *command:
    mosh --ssh="ssh {{ ssh_opts }}" {{ user }}@$(hcloud server ip {{ server }}) {{ if command == "" { "" } else { "-- " + command } }}

[doc("Run a command on the server over SSH, stdin passed through; no command opens a shell")]
[group('act')]
ssh user="root" *command:
    ssh {{ ssh_opts }} {{ user }}@$(hcloud server ip {{ server }}) {{ command }}

[doc("Install the lab session's marketplaces and plugins on the server, from the skill's plugins.sh")]
[group('act')]
plugins:
    ssh {{ ssh_opts }} lab@$(hcloud server ip {{ server }}) 'bash -l -s' < .claude/skills/run-hetzner-lab/plugins.sh

[doc("Forget the host key held for the server's current address, once a new server takes the address over")]
[group('act')]
forget-host:
    ssh-keygen -R "$(hcloud server ip {{ server }})" -f {{ known_hosts }}

[doc("Show whether the deadline backstop is loaded in launchd; fails when it is not")]
[group('read')]
backstop-status:
    launchctl list | grep com.estafette.lab-backstop

[doc("Run the estafette binary on this machine with the lab's directories under LAB_ROOT, e.g. just remote::estafette show-peers lab")]
[group('lab')]
[positional-arguments]
estafette *arguments:
    umask 077 && mkdir -p {{ lab_root }}/run && chmod 0700 {{ lab_root }}
    env {{ lab_directories }} {{ lab_binary }} "$@"

[doc("Serve the card as a lab member on this machine until SIGTERM, printing its init envelope; its run record goes to stderr")]
[group('lab')]
member card:
    umask 077 && mkdir -p {{ lab_root }}/run && chmod 0700 {{ lab_root }}
    rm -f {{ lab_root }}/{{ card }}.control
    env {{ lab_directories }} python3 -I just/remote/member.py serve {{ lab_binary }} {{ lab_network }} {{ card }} {{ lab_root }}/{{ card }}.control

[doc("Call one tool of the lab member serving the card on this machine, e.g. just remote::call mac send '{\"to\":\"srv\",\"body\":\"hi\"}'")]
[group('lab')]
call card tool arguments="{}":
    python3 -I just/remote/member.py call {{ lab_root }}/{{ card }}.control {{ tool }} '{{ arguments }}'

[doc("Serve the card as a lab member on the server, as a systemd unit run by lab; its envelope and run record go to /home/lab/<card>.out and .err")]
[group('lab')]
member-on-server card:
    ssh {{ ssh_opts }} root@$(hcloud server ip {{ server }}) systemd-run --collect --unit={{ member_unit_prefix }}{{ card }} --uid=lab --working-directory={{ remote_tree }} --property=StandardOutput=truncate:/home/lab/{{ card }}.out --property=StandardError=truncate:/home/lab/{{ card }}.err just remote::member {{ card }}

[doc("Stop the card's lab member on the server cleanly, waiting out its departure")]
[group('lab')]
stop-member-on-server card:
    ssh {{ ssh_opts }} root@$(hcloud server ip {{ server }}) systemctl stop {{ member_unit_prefix }}{{ card }}

[doc("Print each frame of the inbox socket at the path init returned, stamped with this machine's clock")]
[group('lab')]
attach inbox_path:
    python3 -I just/remote/member.py attach {{ inbox_path }}

[doc("Refuse, naming the missing path, unless the lab binary exists")]
[private]
lab-binary-built:
    test -x {{ lab_binary }} || { echo "the lab binary {{ lab_binary }} does not exist: build it (cargo build -p estafette makes the default) or point LAB_BINARY at one" >&2; exit 1; }

[doc("Create the lab server with its 24-hour deadline, or reuse it, wait for its setup, then push the committed tree and build the release binary with the relay there")]
[group('lab')]
server-up:
    #!/usr/bin/env bash
    set -euo pipefail
    just remote::backstop-status >&2
    expires="$(date -v+24H +%s)"
    if ! hcloud ssh-key describe {{ lab_resource_name }} > /dev/null 2>&1; then
        hcloud ssh-key create --name {{ lab_resource_name }} --public-key-from-file {{ key }}.pub --label owner=estafette --label expires="$expires" >&2
    fi
    if ! hcloud firewall describe {{ lab_resource_name }} > /dev/null 2>&1; then
        hcloud firewall create --name {{ lab_resource_name }} --rules-file {{ lab_skill }}/firewall-rules.json --label owner=estafette --label expires="$expires" >&2
    fi
    if ! hcloud server describe {{ server }} > /dev/null 2>&1; then
        setup_file="$(mktemp)"
        python3 -I {{ lab_skill }}/fill-setup.py "$setup_file" >&2
        hcloud server create --name {{ server }} --type {{ lab_server_type }} --image {{ lab_image }} --location {{ lab_location }} --ssh-key {{ lab_resource_name }} --firewall {{ lab_resource_name }} --user-data-from-file "$setup_file" --label owner=estafette --label expires="$expires" >&2
        rm -f "$setup_file"
        for address_id in $(hcloud server describe {{ server }} -o json | python3 -I -c 'import json, sys; network = json.load(sys.stdin)["public_net"]; print(*(network[family]["id"] for family in ("ipv4", "ipv6") if network[family]))'); do
            python3 -I {{ lab_skill }}/lab-backstop.py label primary-ip "$address_id" >&2
        done
        ssh-keygen -R "$(hcloud server ip {{ server }})" -f {{ known_hosts }} > /dev/null 2>&1 || true
    fi
    address="$(hcloud server ip {{ server }})"
    for try in $(seq {{ lab_setup_wait_tries }}); do
        if ssh {{ ssh_opts }} -o ConnectTimeout=5 root@"$address" test -e /var/lib/lab-setup-done 2> /dev/null; then
            break
        fi
        if [ "$try" = {{ lab_setup_wait_tries }} ]; then
            echo "the setup of {{ server }} did not finish within {{ lab_setup_wait_tries }} tries of {{ lab_setup_poll_seconds }} s" >&2
            exit 1
        fi
        sleep {{ lab_setup_poll_seconds }}
    done
    ssh {{ ssh_opts }} root@"$address" cloud-init status --long >&2
    git archive HEAD | ssh {{ ssh_opts }} lab@"$address" 'mkdir -p {{ remote_tree }} && tar -x -C {{ remote_tree }}'
    ssh {{ ssh_opts }} lab@"$address" 'bash -l -c "cd {{ remote_tree }} && cargo build --release -p estafette --features relay"' >&2

[doc("Bring the server up and run the network's relay and bootstrap node there as a systemd unit, reusing a live one; prints the host address line; a second host starts after host-up so it joins the live host A, and restarts after A restarts; refuses before creating anything when the lab binary is missing")]
[group('lab')]
infrastructure: lab-binary-built server-up
    #!/usr/bin/env bash
    set -euo pipefail
    address="$(hcloud server ip {{ server }})"
    domain="${address//./-}.sslip.io"
    remote() { ssh {{ ssh_opts }} root@"$address" "$@"; }
    test -e {{ lab_config_file }} || just remote::estafette network create {{ lab_network }} >&2
    remote 'umask 077 && mkdir -p {{ remote_host_root }}/run $(dirname {{ remote_host_config_file }}) && chmod 0700 {{ remote_host_root }} && { test -e {{ remote_host_config_file }} || cat > {{ remote_host_config_file }}; }' < {{ lab_config_file }}
    if ! remote test -e {{ remote_host_certificate_file }}; then
        remote 'env {{ remote_host_directories }} {{ remote_binary }} run-infrastructure {{ lab_network }} '"$address $domain"' > {{ remote_host_line_file }} 2> /dev/null || test -s {{ remote_host_line_file }}'
        certificate="$(just remote::estafette network certify {{ lab_network }} "$(remote cat {{ remote_host_line_file }})" 2> /dev/null)" || { echo "certifying the host of {{ server }} failed: no certificate was written" >&2; exit 1; }
        printf '%s\n' "$certificate" | remote 'umask 077 && cat > {{ remote_host_certificate_temporary }} && mv {{ remote_host_certificate_temporary }} {{ remote_host_certificate_file }}'
    fi
    host_line="$(remote cat {{ remote_host_line_file }})"
    LAB_SERVER={{ server }} just remote::host-start >&2
    printf '%s\n' "$host_line"

[doc("Bring host A up for the lab network: drop every host of an earlier stretch from this machine's owner configuration, start A, add A as its only host, and record the invitation in the root checkout's lab/host.txt alone; two hosts run in this order: host-up for A first, then LAB_SERVER=<name> just remote::infrastructure for B, which joins the live A, and after A restarts restart B, since a host does not rejoin a peer host that restarts; refuses before creating anything when the lab binary is missing")]
[group('lab')]
host-up: lab-binary-built
    #!/usr/bin/env bash
    set -euo pipefail
    umask 077
    if test -e {{ lab_config_file }}; then
        awk '/^(\[\[hosts\]\]|hosts )/ { exit } { print }' {{ lab_config_file }} > {{ lab_config_file }}.tmp
        printf 'hosts = []\n' >> {{ lab_config_file }}.tmp
        mv {{ lab_config_file }}.tmp {{ lab_config_file }}
    fi
    host_line="$(just remote::infrastructure)"
    address="$(hcloud server ip {{ server }})"
    just remote::estafette network add-host {{ lab_network }} "$host_line" >&2
    invitation="$(just remote::estafette network show-invitation {{ lab_network }} 2> /dev/null)"
    record="$(just remote::host-record-path)"
    printf 'network: %s\ninvitation: %s\nhost: %s\nserver: %s %s\ndomain: %s\nwritten: %s\n' {{ lab_network }} "$invitation" "$host_line" {{ server }} "$address" "${address//./-}.sslip.io" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$record"
    chmod 0600 "$record"
    printf 'wrote %s for %s %s\n' "$record" {{ server }} "$address" >&2

[doc("Print the path of the file that holds the lab host's invitation: lab/host.txt of the root checkout, or LAB_HOST_RECORD")]
[private]
host-record-path:
    #!/usr/bin/env bash
    set -euo pipefail
    printf '%s\n' "${LAB_HOST_RECORD:-$({{ root_checkout_command }})/{{ host_record_under_root }}}"

[doc("Check that lab/host.txt holds an invitation, printing none of it; fails naming the file when it does not")]
[group('lab')]
invitation:
    #!/usr/bin/env bash
    set -euo pipefail
    record="$(just remote::host-record-path)"
    grep -q '^invitation: .' "$record" 2> /dev/null || { echo "$record holds no invitation: run just remote::host-up" >&2; exit 1; }

[doc("List every lab resource in the Hetzner project, to read before any creation")]
[group('read')]
list:
    hcloud server list
    hcloud primary-ip list
    hcloud firewall list
    hcloud ssh-key list

[doc("Write the lab network's configuration on the server as lab from stdin, replacing the one there; it holds the network secret, so it travels on stdin alone")]
[group('lab')]
network-on-server:
    ssh {{ ssh_opts }} lab@$(hcloud server ip {{ server }}) 'cd {{ remote_tree }} && just remote::network-from-input'

[doc("Write the lab network's configuration on this machine from stdin, replacing the one there")]
[group('lab')]
network-from-input:
    umask 077 && mkdir -p {{ lab_root }}/run "$(dirname {{ lab_config_file }})" && chmod 0700 {{ lab_root }} && cat > {{ lab_config_file }}.new && mv {{ lab_config_file }}.new {{ lab_config_file }}

[doc("Create a card on the server's lab network as lab, printing its public key")]
[group('lab')]
card-on-server name handle:
    ssh {{ ssh_opts }} lab@$(hcloud server ip {{ server }}) 'cd {{ remote_tree }} && just remote::estafette create-card {{ lab_network }} {{ name }} {{ handle }}'

[doc("Call one tool of the lab member serving the card on the server, its arguments as JSON on stdin; prints the envelope stamped with the server's clock")]
[group('lab')]
call-on-server card tool:
    ssh {{ ssh_opts }} lab@$(hcloud server ip {{ server }}) 'cd {{ remote_tree }} && just remote::call {{ card }} {{ tool }} "$(cat)"'

[doc("Print the run record of the card's lab member on the server")]
[group('lab')]
run-record-on-server card:
    ssh {{ ssh_opts }} lab@$(hcloud server ip {{ server }}) cat /home/lab/{{ card }}.err

relay_only_mark := "estafette-relay-only"

[doc("Drop UDP between the server and this machine, IPv4 to and from this machine's address as the server sees it and every IPv6 datagram but DNS, so members on the two reach each other through a relay alone; relay-paths-restored undoes it")]
[group('lab')]
relay-only-to-here:
    #!/usr/bin/env bash
    set -euo pipefail
    ssh {{ ssh_opts }} root@"$(hcloud server ip {{ server }})" bash -s <<'RULES'
    set -euo pipefail
    here="${SSH_CLIENT%% *}"
    iptables -I INPUT -s "$here" -p udp -m comment --comment {{ relay_only_mark }} -j DROP
    iptables -I OUTPUT -d "$here" -p udp -m comment --comment {{ relay_only_mark }} -j DROP
    ip6tables -I INPUT -p udp ! --sport 53 -m comment --comment {{ relay_only_mark }} -j DROP
    ip6tables -I OUTPUT -p udp ! --dport 53 -m comment --comment {{ relay_only_mark }} -j DROP
    RULES

[doc("Print each rule relay-only-to-here put on the server with its drop counters, then delete every one of them; does nothing when none is there")]
[group('lab')]
relay-paths-restored:
    #!/usr/bin/env bash
    set -euo pipefail
    ssh {{ ssh_opts }} root@"$(hcloud server ip {{ server }})" bash -s <<'RULES'
    set -uo pipefail
    for table in iptables ip6tables; do
        for chain in INPUT OUTPUT; do
            "$table" -L "$chain" -v -n | grep -- {{ relay_only_mark }} | sed "s/^/$table $chain /" || true
            while rule="$("$table" -S "$chain" | grep -m 1 -- '--comment {{ relay_only_mark }}')"; do
                "$table" ${rule/-A/-D} || exit 1
            done
        done
    done
    RULES

[doc("Stop the network's host on the server when it runs, relay and bootstrap node together; infrastructure starts it again")]
[group('lab')]
host-down:
    ssh {{ ssh_opts }} root@$(hcloud server ip {{ server }}) 'if systemctl is-active --quiet {{ host_unit }}; then systemctl stop {{ host_unit }}; fi'

[doc("Start the network's host on the server unless it runs, as infrastructure set it up there, refusing with the server's name when hcloud reports the server not found and with hcloud's own error when describing it fails otherwise; creates, pushes and builds nothing")]
[group('lab')]
host-start:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! describe_failure="$(hcloud server describe {{ server }} 2>&1 > /dev/null)"; then
        if [[ "$describe_failure" == *[Ss]"erver not found: {{ server }}"* ]]; then
            echo "no lab server {{ server }}: host-start starts a host only on a server the run already brought up and creates none" >&2
        else
            printf '%s\n' "$describe_failure" >&2
        fi
        exit 1
    fi
    address="$(hcloud server ip {{ server }})"
    domain="${address//./-}.sslip.io"
    remote() { ssh {{ ssh_opts }} root@"$address" "$@"; }
    if ! remote systemctl is-active --quiet {{ host_unit }}; then
        remote systemctl reset-failed {{ host_unit }} 2> /dev/null || true
        remote systemd-run --unit={{ host_unit }} $(printf -- '--setenv=%s ' {{ remote_host_directories }}) {{ remote_binary }} run-infrastructure {{ lab_network }} "$address" "$domain" >&2
    fi

[doc("Delete every lab resource of the stretch, servers first then firewall and key (primary IPs go with their servers), through the skill's backstop script, and end by printing just remote::list")]
[group('lab')]
down:
    #!/usr/bin/env bash
    set -euo pipefail
    for kind in server firewall ssh-key; do
        resource_ids="$(hcloud "$kind" list --selector {{ lab_owner_label }} --output noheader --output columns=id)"
        for resource_id in $resource_ids; do
            python3 -I {{ lab_skill }}/lab-backstop.py extend "$kind" "$resource_id" 0
        done
    done
    python3 -I {{ lab_skill }}/lab-backstop.py check
    just remote::list
