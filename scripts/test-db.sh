#!/usr/bin/env bash
set -euo pipefail

port=${CANTERLOT_TEST_PG_PORT:-55440}
if ! [[ "$port" =~ ^[1-9][0-9]*$ ]] || (( port < 1024 || port > 65535 )); then
    printf 'CANTERLOT_TEST_PG_PORT must be an integer in 1024..65535\n' >&2
    exit 2
fi
if [[ "$port" == "55440" ]]; then
    container=canterlot-test-pg
else
    container=canterlot-test-pg-$port
fi

action=${1:-}
if (( $# > 0 )); then
    shift
fi

wait_ready() {
    local attempt
    for (( attempt = 0; attempt < 60; attempt++ )); do
        if docker exec "$container" pg_isready -U canterlot -d canterlot >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    return 1
}

case "$action" in
    up)
        # Бюджет соединений на поток тестов: пул теста (5), служебное соединение
        # шаблона (1) и отдельные пулы внутри тестов (до 24). Замер полного прогона на
        # 36 потоках дал 434 соединения при лимите 500 — запаса на раннер с большим
        # числом ядер не было. 20 на поток покрывает запас и растёт вместе с потоками.
        max_connections=$(($(nproc) * 20))
        ((max_connections < 500)) && max_connections=500
        recreate=0
        if docker container inspect "$container" >/dev/null 2>&1; then
            docker start "$container" >/dev/null
            # max_connections читается только при старте postgres: контейнер со старым
            # лимитом пересоздаём. Данные тестовой базы лежат в tmpfs и не нужны.
            if wait_ready; then
                current=$(docker exec "$container" psql -U canterlot -d canterlot \
                    -tAc 'show max_connections' 2>/dev/null || true)
                [[ "$current" == "$max_connections" ]] || recreate=1
            else
                recreate=1
            fi
            if ((recreate)); then
                docker rm --force "$container" >/dev/null
            fi
        else
            recreate=1
        fi
        if ((recreate)); then
            docker run --detach --name "$container" \
                --publish "127.0.0.1:${port}:5432" \
                --env POSTGRES_USER=canterlot \
                --env POSTGRES_PASSWORD=canterlot \
                --env POSTGRES_DB=canterlot \
                --tmpfs /var/lib/postgresql \
                postgres:18.6-trixie -c max_connections="$max_connections" >/dev/null
        fi
        if wait_ready; then
            printf 'PostgreSQL is ready on 127.0.0.1:%s (max_connections=%s)\n' "$port" "$max_connections"
            exit 0
        fi
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
