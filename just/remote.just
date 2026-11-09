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
lab_binary := env("LAB_BINARY", "target/release/estafette")
lab_network := "lab"
lab_directories := "XDG_CONFIG_HOME=" + lab_root / "config" + " XDG_DATA_HOME=" + lab_root / "data" + " XDG_RUNTIME_DIR=" + lab_root / "run"

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
    mkdir -p -m 0700 {{ lab_root }}/run
    env {{ lab_directories }} {{ lab_binary }} "$@"

[doc("Serve the card as a lab member on this machine until SIGTERM, printing its init envelope; its run record goes to stderr")]
[group('lab')]
member card:
    mkdir -p -m 0700 {{ lab_root }}/run
    rm -f {{ lab_root }}/{{ card }}.control
    env {{ lab_directories }} python3 -I just/remote/member.py serve {{ lab_binary }} {{ lab_network }} {{ card }} {{ lab_root }}/{{ card }}.control

[doc("Call one tool of the lab member serving the card on this machine, e.g. just remote::call mac send '{\"to\":\"srv\",\"body\":\"hi\"}'")]
[group('lab')]
call card tool arguments="{}":
    python3 -I just/remote/member.py call {{ lab_root }}/{{ card }}.control {{ tool }} '{{ arguments }}'

[doc("Print each frame of the inbox socket at the path init returned, stamped with this machine's clock")]
[group('lab')]
attach inbox_path:
    python3 -I just/remote/member.py attach {{ inbox_path }}

[doc("List every lab resource in the Hetzner project, to read before any creation")]
[group('read')]
list:
    hcloud server list
    hcloud primary-ip list
    hcloud firewall list
    hcloud ssh-key list
