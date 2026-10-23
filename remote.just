# Lab server on Hetzner: terminal, shell, and the backstop check.
# Every recipe looks the server's address up by name from the live API at run
# time, so a replaced server or a new address needs no edit here.
# Another server: LAB_SERVER=<name> just remote::<recipe>.

server := env("LAB_SERVER", "estafette-lab")
key := env("LAB_KEY", home_directory() / ".ssh" / "estafette_lab")
known_hosts := home_directory() / ".ssh" / "known_hosts_estafette_lab"
ssh_opts := "-i " + key + " -o IdentitiesOnly=yes -o UserKnownHostsFile=" + known_hosts + " -o StrictHostKeyChecking=accept-new"

# List this module's recipes (what bare `just remote` runs)
help:
    @just --list remote

# Print the server's current IPv4 address, from the live API
ip:
    @hcloud server ip {{ server }}

# Open a mosh terminal on the server; extra words run there, e.g. tmux attach -t lab
mosh user="lab" *command:
    mosh --ssh="ssh {{ ssh_opts }}" {{ user }}@$(hcloud server ip {{ server }}) {{ if command == "" { "" } else { "-- " + command } }}

# Run a command on the server over SSH, stdin passed through; no command opens a shell
ssh user="root" *command:
    ssh {{ ssh_opts }} {{ user }}@$(hcloud server ip {{ server }}) {{ command }}

# Install the lab session's marketplaces and plugins on the server, from the skill's plugins.sh
plugins:
    ssh {{ ssh_opts }} lab@$(hcloud server ip {{ server }}) 'bash -l -s' < .claude/skills/run-hetzner-lab/plugins.sh

# Forget the host key held for the server's current address, once a new server takes the address over
forget-host:
    ssh-keygen -R "$(hcloud server ip {{ server }})" -f {{ known_hosts }}

# Show whether the deadline backstop is loaded in launchd; fails when it is not
backstop-status:
    launchctl list | grep com.estafette.lab-backstop

# List every lab resource in the Hetzner project, to read before any creation
list:
    hcloud server list
    hcloud primary-ip list
    hcloud firewall list
    hcloud ssh-key list
