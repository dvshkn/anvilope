#!/bin/bash

IMAGE_NAME=greenmail/standalone:2.1.7
GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled -Dgreenmail.verbose'

podman run -t -i -e GREENMAIL_OPTS="$GREENMAIL_OPTS" \
    -p 3025:3025 \
    -p 3110:3110 \
    -p 3143:3143 \
    -p 3465:3465 \
    -p 3993:3993 \
    -p 3995:3995 \
    -p 8081:8080 \
    docker.io/$IMAGE_NAME
