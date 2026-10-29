#!/usr/bin/env bash
set -euo pipefail

container=canterlot-test-pg
action=${1:-}
if (( $# > 0 )); then
    shift
fi

case "$action" in
    up)
        if docker container inspect "$container" >/dev/null 2>&1; then
            docker start "$container" >/dev/null
        else
            docker run --detach --name "$container" \
                --publish 127.0.0.1:55440:5432 \
                --env POSTGRES_USER=canterlot \
                --env POSTGRES_PASSWORD=canterlot \
                --env POSTGRES_DB=canterlot \
                --tmpfs /var/lib/postgresql \
                postgres:18.6-trixie -c max_connections=500 >/dev/null
        fi
        for (( attempt = 0; attempt < 60; attempt++ )); do
            if docker exec "$container" pg_isready -U canterlot -d canterlot >/dev/null 2>&1; then
                printf 'PostgreSQL is ready on 127.0.0.1:55440\n'
                exit 0
            fi
            sleep 1
        done
        printf 'PostgreSQL did not become ready within 60 seconds\n' >&2
        exit 1
        ;;
    down)
        docker rm --force "$container"
        ;;
    psql)
        docker exec --interactive --tty "$container" psql -U canterlot -d canterlot "$@"
        ;;
    *)
        printf 'Usage: %s {up|down|psql [args...]}\n' "$0" >&2
        exit 2
        ;;
esac
