#!/bin/sh
# deploy on C (files were scp'd to /tmp)
mkdir -p /root/e24
cd /root/e24
cp /tmp/ctl.sh /tmp/iroh-relay /tmp/e24-cross-linux .
cp /tmp/relay-c.toml relay.toml
mv e24-cross-linux e24-cross
chmod +x ctl.sh e24-cross iroh-relay
ls
