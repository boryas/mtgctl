#!/usr/bin/env bash
ssh bur.io "cd ~/mtgctl-web && ./bin/mtgctl-web --data /mnt/mtgctl_data/data ref-list" 2>&1 | grep -v "post-quantum\|store now\|openssh.com/pq"
