#!/bin/bash
set -e

# Set up directories and permissions
mkdir -p /home/rdrive/.rdrive/storage
chmod -R 755 /home/rdrive/.rdrive

# Start the server
# shellcheck disable=SC2145
echo "Starting r-drive with args: $@"
exec /app/rds "$@"


