#!/bin/sh
set -eu

: "${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD}"
backup_dir=${BACKUP_DIR:-./backups}
mkdir -p "$backup_dir"
file="$backup_dir/graphwar-$(date -u +%Y%m%dT%H%M%SZ).sql.gz"

docker compose -f deploy/compose.yaml exec -T postgres \
  pg_dump -U graphwar -d graphwar | gzip -9 > "$file"

find "$backup_dir" -type f -name 'graphwar-*.sql.gz' -mtime +14 -delete
printf '%s\n' "$file"
