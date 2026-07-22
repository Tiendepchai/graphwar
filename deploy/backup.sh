#!/bin/sh
set -eu

case ${1:-backup} in
  backup)
    [ "$#" -le 1 ] || { printf '%s\n' 'usage: backup.sh [backup|verify FILE]' >&2; exit 64; }
    : "${DOMAIN:?set DOMAIN}"
    : "${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD}"
    case ${COMPOSE_BIN:-auto} in
      auto)
        if command -v docker-compose >/dev/null 2>&1; then
          compose() { docker-compose -f deploy/compose.yaml "$@"; }
        else
          compose() { docker compose -f deploy/compose.yaml "$@"; }
        fi
        ;;
      docker-compose) compose() { docker-compose -f deploy/compose.yaml "$@"; } ;;
      docker) compose() { docker compose -f deploy/compose.yaml "$@"; } ;;
      *) printf '%s\n' 'COMPOSE_BIN must be auto, docker-compose, or docker' >&2; exit 64 ;;
    esac
    backup_dir=${BACKUP_DIR:-./backups}
    mkdir -p "$backup_dir"
    file="$backup_dir/graphwar-$(date -u +%Y%m%dT%H%M%SZ).sql.gz"
    dump=
    compressed=
    cleanup() {
      [ -z "$dump" ] || rm -f "$dump"
      [ -z "$compressed" ] || rm -f "$compressed"
    }
    trap cleanup 0
    trap 'exit 1' HUP INT TERM
    dump=$(mktemp "$backup_dir/.graphwar-dump.XXXXXX")
    compose exec -T postgres pg_dump -U graphwar -d graphwar > "$dump"
    [ -s "$dump" ] || {
      printf '%s\n' 'pg_dump produced no data' >&2
      exit 1
    }
    compressed=$(mktemp "$backup_dir/.graphwar-backup.XXXXXX")
    gzip -9 < "$dump" > "$compressed"
    gzip -t "$compressed"
    ln "$compressed" "$file"
    printf '%s\n' "$file"
    ;;
  verify)
    [ "$#" -eq 2 ] || { printf '%s\n' 'usage: backup.sh verify FILE' >&2; exit 64; }
    gzip -t "$2"
    printf '%s\n' "$2"
    ;;
  *)
    printf '%s\n' 'usage: backup.sh [backup|verify FILE]' >&2
    exit 64
    ;;
esac
