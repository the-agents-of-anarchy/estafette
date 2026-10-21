#!/bin/sh
# deploy on B (run from /root/e24 after scp)
cd /root/e24
mv relay-b.toml relay.toml
cp /root/e22/relay-bin/bin/iroh-relay .
cp -r /root/e22/certs .
chmod +x ctl.sh e24-cross iroh-relay
ls
