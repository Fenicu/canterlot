# shellcheck shell=bash
# Самообёртка проверок в аренду тяжёлых задач воркера (docs/development.md,
# «Очередь тяжёлых задач»). Подключается через source.
#
#   lease_self_wrap KIND LABEL CMD ARGS...
#
# Перезапускает CMD под `canterlot-worker lease run --kind KIND --label LABEL`, если
# аренда ещё не взята. Без бинарника или со старым бинарником (нет подкоманды `lease`)
# печатает одну строку в stderr и возвращается: скрипт идёт дальше без аренды.
# CANTERLOT_NO_LEASE=1 отключает обёртку; CANTERLOT_LEASE_DRY=1 печатает решение и
# завершает скрипт (для тестов классификации).
lease_self_wrap() {
    local kind=$1 label=$2
    shift 2
    local reason="" bin=""
    if [[ -n "${CANTERLOT_LEASE_HELD:-}" ]]; then
        reason=held
    elif [[ "${CANTERLOT_NO_LEASE:-}" == 1 ]]; then
        reason=disabled
    else
        bin="${CANTERLOT_WORKER_BIN:-}"
        if [[ -z "$bin" || ! -x "$bin" ]]; then
            bin=$(command -v canterlot-worker 2>/dev/null || true)
        fi
        if [[ -z "$bin" ]]; then
            reason=no-binary
        elif ! "$bin" lease --help >/dev/null 2>&1; then
            reason=old-binary
        fi
    fi
    if [[ "${CANTERLOT_LEASE_DRY:-}" == 1 ]]; then
        printf 'canterlot lease dry: kind=%s wrap=%s\n' "$kind" "${reason:-yes}"
        exit 0
    fi
    case "$reason" in
        "") exec "$bin" lease run --kind "$kind" --label "$label" -- "$@" ;;
        no-binary)
            echo "canterlot lease: canterlot-worker not found, running without lease" >&2
            export CANTERLOT_LEASE_HELD=unleased
            ;;
        old-binary)
            echo "canterlot lease: $bin has no lease subcommand, running without lease" >&2
            export CANTERLOT_LEASE_HELD=unleased
            ;;
    esac
}
