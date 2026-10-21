#!/bin/sh
# deploy to B and C
cd /tmp/e24 || exit 1
S="-q -i /Users/ryzhakar/.ssh/estafette_lab -o IdentitiesOnly=yes -o UserKnownHostsFile=/Users/ryzhakar/.ssh/known_hosts_estafette_lab"
scp $S ctl.sh relay-b.toml deploy-b.sh root@178.105.88.14:/root/e24/
scp $S ctl.sh relay-c.toml e24-cross-linux deploy-c.sh root@2.29.112.122:/tmp/
./sshB sh /root/e24/deploy-b.sh
./sshC sh /tmp/deploy-c.sh
