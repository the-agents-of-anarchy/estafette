#!/bin/sh
# build e24-cross on C as user lab
mkdir -p /home/lab/e24
tar xzf /tmp/src.tgz -C /home/lab/e24
chown -R lab:lab /home/lab/e24
su - lab -c 'cd /home/lab/e24/e24-cross && rustc --version && cargo build --release 2>&1 | tail -3'
