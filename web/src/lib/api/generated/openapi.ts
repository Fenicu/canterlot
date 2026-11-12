export interface paths {
    "/api/v1/attachments": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_attachments"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/attachments/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_attachments__id_"];
        put?: never;
        post?: never;
        delete: operations["delete__api_v1_attachments__id_"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/attachments/{id}/complete": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_attachments__id__complete"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/attachments/{id}/content": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_attachments__id__content"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head: operations["head__api_v1_attachments__id__content"];
        patch: operations["patch__api_v1_attachments__id__content"];
        trace?: never;
    };
    "/api/v1/auth/logout": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Выход: сеанс отзывается, cookie стирается. Cookie устройства остаётся. */
        post: operations["post__api_v1_auth_logout"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/methods": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Какие способы входа сейчас доступны: кнопка ключа доступа прячется без ключей. */
        get: operations["get__api_v1_auth_methods"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/passkey/finish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Завершение входа по ключу. Неудача (чужой, испорченный, повторный или просроченный
         *     ответ) — 401 `invalid_credentials` и неудача перебора в корзине клиента; общая
         *     блокировка входа по паролю ключи не касается.
         */
        post: operations["post__api_v1_auth_passkey_finish"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/passkey/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Начало входа по ключу: все ключи владельца в `allowCredentials`, проверка пользователя
         *     обязательна. Не больше 10 начал в минуту с корзины и 500 живых обрядов всего; известное
         *     устройство общим пределом не ограничено, атакующий не запирает им вход владельца.
         */
        post: operations["post__api_v1_auth_passkey_start"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/passkeys": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_auth_passkeys"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/passkeys/register/finish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Завершение добавления в том же сеансе, что начал его; сеанс должен быть свеж и сейчас:
         *     обряд живёт 5 минут и мог пережить свежесть.
         */
        post: operations["post__api_v1_auth_passkeys_register_finish"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/passkeys/register/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Начало добавления ключа: нужен свежий сеанс. Уже добавленные ключи в
         *     `excludeCredentials`, `residentKey: preferred`.
         */
        post: operations["post__api_v1_auth_passkeys_register_start"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/passkeys/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Удаление ключа: нужен свежий сеанс. Ответ — оставшиеся ключи. */
        delete: operations["delete__api_v1_auth_passkeys__id_"];
        options?: never;
        head?: never;
        /** Новое имя ключа; свежести не требует. */
        patch: operations["patch__api_v1_auth_passkeys__id_"];
        trace?: never;
    };
    "/api/v1/auth/password": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Задать или сменить пароль. Смена несёт `current_password`; первый пароль требует
         *     свежего сеанса. Успех завершает все остальные сеансы и делает текущий свежим.
         */
        put: operations["put__api_v1_auth_password"];
        /**
         * Вход по логину и паролю. Неверный логин, неверный пароль и отсутствие пароля дают
         *     один и тот же 401 и одну проверку argon2.
         */
        post: operations["post__api_v1_auth_password"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/qr/approve": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Разрешить вход: новое устройство получит сеанс с `approved_by` = текущий сеанс.
         *     Такой сеанс несвеж и завершается вместе с подтвердившим (каскад). Один сеанс держит
         *     не больше 5 живых или ждущих выдачи входов, подтверждённых им.
         */
        post: operations["post__api_v1_auth_qr_approve"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/qr/deny": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_auth_qr_deny"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/qr/inspect": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Открыть код на устройстве с сеансом: запрос переходит в `scanned`, опрос нового
         *     устройства показывает «Код отсканирован».
         */
        post: operations["post__api_v1_auth_qr_inspect"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/qr/poll": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Долгий опрос: до 25 секунд, перепроверка строки каждые 500 мс или по уведомлению
         *     шины (работает между процессами). Опрос, увидевший `approved`, создаёт сеанс; из
         *     двух одновременных его получает ровно один, второй видит `expired`.
         */
        post: operations["post__api_v1_auth_qr_poll"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/qr/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Новый код входа на 60 секунд. Не больше 10 запусков в минуту с одной корзины адресов
         *     и 20 ожидающих кодов всего; сверх предела 429 сразу.
         */
        post: operations["post__api_v1_auth_qr_start"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/reauth/passkey/finish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Завершение повторного входа: сеанс свеж 10 минут. Неудача — 403 `passkey_rejected`
         *     и неудача перебора (сеанс при этом жив).
         */
        post: operations["post__api_v1_auth_reauth_passkey_finish"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/reauth/passkey/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Повторный вход ключом: обряд привязан к текущему сеансу. */
        post: operations["post__api_v1_auth_reauth_passkey_start"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/reauth/password": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Повторный вход паролем: сеанс становится свежим на 10 минут. То же ключом доступа —
         *     `/reauth/passkey/*` (`http::signin_passkeys`).
         */
        post: operations["post__api_v1_auth_reauth_password"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/recovery": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Вход по ссылке восстановления: одноразовый токен из `canterlot-hub auth recovery-link`. */
        post: operations["post__api_v1_auth_recovery"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/sessions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_auth_sessions"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/sessions/revoke-others": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** «Завершить все остальные сеансы». Требует свежего сеанса. */
        post: operations["post__api_v1_auth_sessions_revoke_others"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/sessions/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Завершает сеанс. Свой сеанс — это выход; чужой требует свежего сеанса и уносит
         *     каскадом сеансы, вход которых он подтвердил.
         */
        delete: operations["delete__api_v1_auth_sessions__id_"];
        options?: never;
        head?: never;
        patch: operations["patch__api_v1_auth_sessions__id_"];
        trace?: never;
    };
    "/api/v1/auth/settings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** Срок бездействия для всех сеансов, включая текущий; cookie текущего получает новый срок. */
        patch: operations["patch__api_v1_auth_settings"];
        trace?: never;
    };
    "/api/v1/bookmarks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_bookmarks"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/config": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Лимиты и пороги, которыми клиент проверяет ввод до отправки. */
        get: operations["get__api_v1_config"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations"];
        put?: never;
        post: operations["post__api_v1_conversations"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id_"];
        put?: never;
        post?: never;
        delete: operations["delete__api_v1_conversations__id_"];
        options?: never;
        head?: never;
        patch: operations["patch__api_v1_conversations__id_"];
        trace?: never;
    };
    "/api/v1/conversations/{id}/accept": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__accept"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/alarms": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__alarms"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/alarms/{alarm_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["delete__api_v1_conversations__id__alarms__alarm_id_"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/blocks/{block_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__blocks__block_id_"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/bookmarks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__bookmarks"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/bookmarks/{kind}/{target_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put: operations["put__api_v1_conversations__id__bookmarks__kind___target_id_"];
        post?: never;
        delete: operations["delete__api_v1_conversations__id__bookmarks__kind___target_id_"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/compact": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__compact"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/crunch": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put: operations["put__api_v1_conversations__id__crunch"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/delegation": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__delegation"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/draft": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__draft"];
        put: operations["put__api_v1_conversations__id__draft"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/executions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__executions"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/groups": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put: operations["put__api_v1_conversations__id__groups"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/groups/{group_id}/pin": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put: operations["put__api_v1_conversations__id__groups__group_id__pin"];
        post?: never;
        delete: operations["delete__api_v1_conversations__id__groups__group_id__pin"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/history": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__history"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/inputs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__inputs"];
        put?: never;
        post: operations["post__api_v1_conversations__id__inputs"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/inputs/{input_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch: operations["patch__api_v1_conversations__id__inputs__input_id_"];
        trace?: never;
    };
    "/api/v1/conversations/{id}/inputs/{input_id}/send-now": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__inputs__input_id__send_now"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/interrupt": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__interrupt"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/lanes/{block_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__lanes__block_id_"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/nudge": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__nudge"];
        put: operations["put__api_v1_conversations__id__nudge"];
        post?: never;
        delete: operations["delete__api_v1_conversations__id__nudge"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/nudge/events": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__nudge_events"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/queue/continue": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__queue_continue"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/read": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__read"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/requests/{request_id}/decision": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__requests__request_id__decision"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/resolve-owner-lost": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__resolve_owner_lost"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/restore": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__restore"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/resume": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__resume"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/runs/{run_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__runs__run_id_"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/scheduled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__scheduled"];
        put?: never;
        post: operations["post__api_v1_conversations__id__scheduled"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/settings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch: operations["patch__api_v1_conversations__id__settings"];
        trace?: never;
    };
    "/api/v1/conversations/{id}/snapshot": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__snapshot"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/spawn-queue": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__spawn_queue"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/spawn-queue/{queue_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["delete__api_v1_conversations__id__spawn_queue__queue_id_"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/spawn-queue/{queue_id}/start-now": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__spawn_queue__queue_id__start_now"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/spend-reserve": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Ручное продолжение одной сессии за счёт недельного резерва до сброса недели. */
        post: operations["post__api_v1_conversations__id__spend_reserve"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/stop": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__stop"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/trash": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["delete__api_v1_conversations__id__trash"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/conversations/{id}/usage": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_conversations__id__usage"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/events": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_events"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/groups": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_groups"];
        put?: never;
        post: operations["post__api_v1_groups"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/groups/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["delete__api_v1_groups__id_"];
        options?: never;
        head?: never;
        patch: operations["patch__api_v1_groups__id_"];
        trace?: never;
    };
    "/api/v1/inputs/{id}/cancel": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_inputs__id__cancel"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/limit-settings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_limit_settings"];
        put: operations["put__api_v1_limit_settings"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/limits": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_limits"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/limits/history": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_limits_history"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/limits/refresh": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_limits_refresh"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/machines": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_machines"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/machines/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_machines__id_"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/machines/{id}/claude-histories": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_machines__id__claude_histories"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/machines/{id}/owner-instructions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_machines__id__owner_instructions"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/machines/{id}/owner-instructions/file": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_machines__id__owner_instructions_file"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/machines/{id}/workdirs/recent": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_machines__id__workdirs_recent"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/machines/{id}/workdirs/suggest": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_machines__id__workdirs_suggest"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/me": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Личность владельца; PWA привязывает к `owner_id` локальные данные. */
        get: operations["get__api_v1_me"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/media/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_media__id_"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/openapi.json": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Машиночитаемый контракт API (`docs/api/openapi.json`). */
        get: operations["get__api_v1_openapi_json"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/plan": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_plan"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/presets": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_presets"];
        put?: never;
        post: operations["post__api_v1_presets"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/presets/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_presets__id_"];
        put?: never;
        post?: never;
        delete: operations["delete__api_v1_presets__id_"];
        options?: never;
        head?: never;
        patch: operations["patch__api_v1_presets__id_"];
        trace?: never;
    };
    "/api/v1/prompt-suggestion-settings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_prompt_suggestion_settings"];
        put: operations["put__api_v1_prompt_suggestion_settings"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/routing-policy": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_routing_policy"];
        put: operations["put__api_v1_routing_policy"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/routing-policy/preview": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_routing_policy_preview"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/scheduled/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["delete__api_v1_scheduled__id_"];
        options?: never;
        head?: never;
        patch: operations["patch__api_v1_scheduled__id_"];
        trace?: never;
    };
    "/api/v1/search": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["post__api_v1_search"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/snapshot/conversations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_snapshot_conversations"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/snapshot/machines": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_snapshot_machines"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/spawn-whitelist": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_spawn_whitelist"];
        put: operations["put__api_v1_spawn_whitelist"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/ws": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get__api_v1_ws"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/healthz": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Живость процесса: без личности и без обращения к базе. */
        get: operations["get__healthz"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/readyz": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Готовность: база отвечает, а запись установки совпадает с состоянием процесса и манифестом. */
        get: operations["get__readyz"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        AcceptBody: {
            /** @description Убрать рабочую копию, даже если в ней есть незафиксированные изменения. */
            force?: boolean;
        };
        /** @enum {string} */
        AcceptState: "stopping" | "removing" | "done";
        AcceptView: {
            state: components["schemas"]["AcceptState"];
        };
        ActivityConfigView: {
            /** Format: int64 */
            stalled_generating_secs: number;
            /** Format: int64 */
            stalled_tool_secs: number;
        };
        /** @enum {string} */
        ActivityState: "idle" | "working" | "waiting_for_user" | "stalled" | "quota_paused" | "failed" | "stopped";
        ActivityView: {
            evidence: components["schemas"]["Evidence"];
            since: string;
            state: components["schemas"]["ActivityState"];
        };
        /** @description Один сработавший будильник пачки: те же поля, что и у одиночного события. */
        AlarmEvent: {
            /** Format: uuid */
            alarm_id?: string | null;
            alarm_key?: string | null;
            alarm_path?: string | null;
            /** Format: int64 */
            alarm_size?: number | null;
            /** Format: date-time */
            armed_at?: string | null;
            condition?: unknown;
            fired_reason?: string | null;
            note?: string | null;
            sessions?: components["schemas"]["AlarmSession"][] | null;
        };
        AlarmList: {
            alarms: components["schemas"]["AlarmView"][];
        };
        /** @description Одна цель условия `session_state` на момент срабатывания. */
        AlarmSession: {
            /** Format: uuid */
            id: string;
            state: string;
            title: string;
        };
        /** @description То, что видят PWA и HTTP: без заметки и без внутренних отметок. */
        AlarmView: {
            /** Format: uuid */
            alarm_id: string;
            batch: boolean;
            /** Format: double */
            below_percent?: number | null;
            condition: string;
            /** Format: uuid */
            conversation_id: string;
            created_at: string;
            deadline_at?: string | null;
            failure_reason?: string | null;
            fired_at?: string | null;
            fired_reason?: string | null;
            fires_at?: string | null;
            /**
             * Format: double
             * @description Разобранное условие `machine` для подписей PWA на русском.
             */
            free_disk_gb_above?: number | null;
            fresh?: boolean | null;
            key?: string | null;
            kind: string;
            /** Format: double */
            load_per_core_below?: number | null;
            /** Format: double */
            mem_available_gb_above?: number | null;
            /** Format: int64 */
            min_bytes?: number | null;
            note?: string | null;
            /** Format: int64 */
            note_bytes: number;
            only_on_timeout: boolean;
            /** @description Разобранное условие `file` для подписей PWA на русском. */
            path?: string | null;
            /** @description Разобранное условие `limit_reset` для подписей PWA на русском. */
            provider?: string | null;
            read_head?: boolean | null;
            session_ids: string[];
            state: string;
            states: string[];
            window?: string | null;
        };
        /**
         * @description Применённый пресет в эффективных настройках: ссылка, название и версии.
         *     Текста здесь нет — только снимок запуска везёт текст.
         */
        AppliedPreset: {
            /** Format: int64 */
            current_revision?: number | null;
            deleted: boolean;
            id: string;
            kind: components["schemas"]["PresetKind"];
            /** Format: int64 */
            revision?: number | null;
            title: string;
        };
        /** @enum {string} */
        ArchiveReason: "crunch" | "scheduled" | "live_execution";
        AttachmentRecord: {
            /** Format: uuid */
            conversation_id: string;
            fail_reason?: string | null;
            /** Format: uuid */
            id: string;
            mime: string;
            name: string;
            sha256: string;
            /** Format: int64 */
            size: number;
            state: string;
            /** Format: int64 */
            uploaded_bytes?: number | null;
        };
        AttachmentView: {
            /** Format: uuid */
            attachment_id: string;
            image: boolean;
            mime: string;
            name: string;
            sha256: string;
            /** Format: int64 */
            size: number;
        };
        AttributeView: {
            /** @description Сессия принята родителем через `session_accept`: живёт в архиве до срока хранения. */
            accepted?: boolean;
            archived: boolean;
            color?: string | null;
            created_at: string;
            group_pins: components["schemas"]["GroupPinView"][];
            groups: string[];
            harness: components["schemas"]["HarnessRef"];
            /** Format: uuid */
            id: string;
            machine: components["schemas"]["MachineRef"];
            /** Format: uuid */
            parent_id?: string | null;
            pinned: boolean;
            /** Format: int32 */
            spawn_depth?: number;
            title: string;
            title_source: string;
            /** @description Сессия в корзине владельца: видна только на своей странице и во вкладке «Корзина». */
            trashed?: boolean;
            trashed_at?: string | null;
            workdir: string;
            worktree?: components["schemas"]["WorktreeView"] | null;
        };
        Attributes: {
            archived?: boolean | null;
            color?: string | null;
            force?: boolean;
            pinned?: boolean | null;
            title?: string | null;
        };
        AuthSettingsBody: {
            /**
             * Format: int32
             * @description 7, 30, 90, 180 или 365.
             */
            idle_timeout_days: number;
        };
        AuthSettingsView: {
            /** Format: int32 */
            idle_timeout_days: number;
        };
        /** @description Пустой объект на один выпуск: старые вкладки PWA читают `config.auth.logout_url`. */
        AuthView: Record<string, never>;
        BackgroundTaskView: {
            /**
             * Format: uuid
             * @description Блок вызова Agent или Bash.
             */
            block_id?: string | null;
            description: string;
            /** Format: date-time */
            ended_at?: string | null;
            /** @description `agent` или `bash`. */
            kind: string;
            parent_task_id?: string | null;
            /** Format: date-time */
            started_at: string;
            /** @description `running`, `completed`, `failed` или `stopped`. */
            state: string;
            task_id: string;
        };
        /**
         * Format: binary
         * @description Двоичное содержимое файла.
         */
        BinaryContent: string;
        BlockPage: {
            checksum?: string | null;
            content: string;
            /** Format: int64 */
            next_offset?: number | null;
            /** Format: int64 */
            offset: number;
            /** Format: int64 */
            rev: number;
            state: string;
            /** Format: int64 */
            total_bytes: number;
        };
        BlockView: {
            agent: string;
            /**
             * @description Set to `"cli_policy"` when the tool ran without an `approve` prompt
             *     while the effective mode was not `bypassPermissions`.
             */
            approved_by?: string | null;
            block_kind: string;
            content: string;
            /** Format: int64 */
            content_bytes: number;
            content_truncated: boolean;
            created_at: string;
            created_seq: string;
            /** @description Файлы, которые агент отдал владельцу; появляются после `block_file_attached`. */
            files: components["schemas"]["FileView"][];
            finished_at?: string | null;
            /** Format: uuid */
            id: string;
            incomplete_reason?: string | null;
            /** Format: uuid */
            input_command_id?: string | null;
            /** Format: int64 */
            line_count: number;
            /** @description Картинки, на которые смотрел агент; появляются после `block_media_attached`. */
            media: components["schemas"]["MediaView"][];
            model: string;
            /** Format: uuid */
            parent_block_id?: string | null;
            /** Format: int64 */
            rev: number;
            revision_gap: boolean;
            started_at: string;
            state: string;
            summary?: string | null;
            tool_name?: string | null;
        };
        BookmarkPage: {
            bookmarks: components["schemas"]["BookmarkView"][];
            next_cursor: string | null;
        };
        /**
         * @description Закладка с полями перехода, как у совпадения поиска: `block_id` или `input_id` и `seq`
         *     открывают ленту на нужном месте, `jump_seq` — место верхнего предка (у закладки это
         *     всегда сама цель: вложенные блоки не бронируются).
         */
        BookmarkView: {
            agent: string | null;
            archived: boolean;
            /** Format: uuid */
            block_id: string | null;
            /** Format: uuid */
            conversation_id: string;
            conversation_title: string;
            created_at: string;
            excerpt: string;
            harness: string;
            /** Format: uuid */
            id: string;
            /** Format: uuid */
            input_id: string | null;
            /** Format: int64 */
            jump_seq: number;
            kind: components["schemas"]["HitKind"];
            model: string;
            origin_kind: components["schemas"]["OriginKind"] | null;
            /** @description `owner` у ввода и `agent` у блока; имя агента — в `agent`. */
            role: string;
            /** Format: int64 */
            seq: number;
            target_created_at: string;
            /** Format: uuid */
            target_id: string;
            target_kind: components["schemas"]["TargetKind"];
        };
        CancelOutcome: "cancelled" | {
            cancel_requested: {
                /** Format: uuid */
                cancel_command_id: string;
            };
        } | {
            too_late: {
                state: components["schemas"]["InputState"];
            };
        };
        /** @description Кандидат типа задачи. */
        Candidate: {
            effort?: string | null;
            exclude_workdirs?: string[];
            harness: string;
            levels?: string[] | null;
            model: string;
            nudge?: unknown;
            preset?: string | null;
            /** Format: int64 */
            watchdog_minutes?: number | null;
        };
        ClaudeHistoryList: {
            histories: components["schemas"]["ClaudeHistoryView"][];
        };
        ClaudeHistoryView: {
            id: string;
            /** Format: int64 */
            modified_at_ms: number;
            preview: string;
            /** Format: int64 */
            size: number;
        };
        CompactOutcome: {
            requested: {
                /** Format: uuid */
                command_id: string;
            };
        } | {
            already_requested: {
                /** Format: uuid */
                command_id: string;
            };
        };
        /**
         * @description Ход сжатия контекста, запрошенного владельцем.
         * @enum {string}
         */
        CompactionProgress: "requested" | "failed";
        /** @description Строка ленты сжатия. */
        CompactionView: {
            created_at: string;
            created_seq?: string | null;
            finished_at?: string | null;
            /** Format: uuid */
            id: string;
            /** @description `None` — сжатие ещё идёт. */
            ok?: boolean | null;
            /** Format: int64 */
            pre_tokens?: number | null;
            trigger: string;
        };
        CompleteBody: {
            /** @description Контрольная сумма, с которой клиент завершает загрузку; другая сумма даёт конфликт. */
            sha256?: string | null;
        };
        ConfigView: {
            activity: components["schemas"]["ActivityConfigView"];
            auth: components["schemas"]["AuthView"];
            defaults: components["schemas"]["DefaultsView"];
            limit_thresholds: components["schemas"]["LimitThresholdsView"];
            limits: components["schemas"]["LimitsView"];
            machines: components["schemas"]["MachinesView"];
            /** Format: int32 */
            schema_version: number;
            sessions: components["schemas"]["SessionsView"];
            version: string;
            ws: components["schemas"]["WsView"];
        };
        /** @description Кандидат в ответе: пара, вердикт и короткая английская причина. */
        Considered: {
            harness: string;
            model: string;
            reason: string;
            verdict: string;
        };
        /**
         * @description Блок содержимого ввода. Владелец присылает только `text`; блоки `attachment` хаб добавляет
         *     сам после текста при привязке готовых вложений.
         */
        ContentBlock: {
            text: string;
            /** @enum {string} */
            type: "text";
        } | {
            /** Format: uuid */
            attachment_id: string;
            image: boolean;
            mime: string;
            name: string;
            sha256: string;
            /** Format: int64 */
            size: number;
            /** @enum {string} */
            type: "attachment";
        };
        ContextInfo: {
            /** Format: int64 */
            tokens: number;
            /** Format: int64 */
            window_tokens?: number | null;
        };
        /**
         * @description Последний известный расход контекста разговора для индикатора: живое исполнение
         *     или свежий законченный ход. `None` — данных ещё нет, индикатор скрыт.
         */
        ContextSnapshot: {
            /** Format: int64 */
            tokens: number;
            /** Format: int64 */
            window_tokens?: number | null;
        };
        ContinueOutcome: {
            remaining: components["schemas"]["PauseReason"][];
            removed: components["schemas"]["PauseReason"][];
        };
        ConversationDetail: components["schemas"]["AttributeView"] & {
            activity: components["schemas"]["ActivityView"];
            /** @description Начало продолженной истории Claude Code; тихая первая строка ленты. */
            continued_from_at?: string | null;
            crunch: components["schemas"]["CrunchDetail"];
            effort: string;
            execution?: components["schemas"]["LiveExecutionView"] | null;
            holds: components["schemas"]["HoldView"][];
            last_activity_at?: string | null;
            model: string;
            /**
             * @description Конкретная модель за псевдонимом `model` (`fable[1m]` → `claude-fable-5-2`): из ходов
             *     сессии, до первого хода на этой модели — последнее наблюдение на машине; `None` — неизвестно.
             */
            model_resolved?: string | null;
            permission_mode: string;
            queue: components["schemas"]["QueueView"];
            /** @description Решение маршрута, выбравшее исполнителя; `None` — запуск без маршрута. */
            routing?: unknown;
            settings: components["schemas"]["SettingsView"];
            /** @description Принятый ход записан: следующий запуск продолжит беседу, а не начнёт новую. */
            turn_recorded: boolean;
            /** Format: int32 */
            unread_count: number;
        };
        ConversationSnapshot: {
            /** @description Взведённые и недавние завершившиеся будильники разговора. */
            alarms: components["schemas"]["AlarmView"][];
            background: components["schemas"]["BackgroundTaskView"][];
            context?: components["schemas"]["ContextSnapshot"] | null;
            detail: components["schemas"]["ConversationDetail"];
            draft?: components["schemas"]["DraftView"] | null;
            history: components["schemas"]["HistoryPage"];
            nudge?: components["schemas"]["NudgeRuleView"] | null;
            nudge_events: components["schemas"]["NudgeEventView"][];
            /** @description Незавершённые запросы по времени создания: открытые, с сохранённым и с передаваемым агенту решением. */
            open_requests: components["schemas"]["RequestView"][];
            prompt_suggestion?: components["schemas"]["PromptSuggestionView"] | null;
            queue: components["schemas"]["SnapshotQueueView"];
            scheduled: components["schemas"]["ScheduledMessage"][];
            /** @description Очередь запусков этого разговора: ожидающие и недавно завершённые. */
            spawn_queue: components["schemas"]["QueueItemView"][];
            /** @description Списки задач агента по дорожкам, основной агент первым. */
            todos: components["schemas"]["TodoListView"][];
        };
        Counts: {
            /** Format: int64 */
            all: number;
            /** Format: int64 */
            archived: number;
            groups: components["schemas"]["GroupCount"][];
            /** Format: int64 */
            pinned: number;
            /** Format: int64 */
            trashed: number;
        };
        CreateBody: {
            color?: string | null;
            crunch?: components["schemas"]["CrunchSetting"];
            effort: string;
            group_ids?: string[];
            harness: string;
            instruction_profile?: string | null;
            /** Format: uuid */
            machine_id: string;
            model: string;
            permission_mode?: string | null;
            /** @description Продолжить готовую историю Claude Code: только её идентификатор. */
            resume_native_id?: string | null;
            start?: boolean;
            title?: string | null;
            workdir: string;
        };
        Created: {
            /** Format: uuid */
            conversation_id: string;
            execution?: components["schemas"]["StartedExecution"] | null;
        };
        CreatedAttachment: components["schemas"]["AttachmentRecord"] & {
            upload: components["schemas"]["UploadOffer"];
        };
        CrunchDetail: {
            enabled: boolean;
            reserve_override_until?: string | null;
            resume_pending: boolean;
            resume_text?: string | null;
        };
        CrunchSetting: {
            enabled: boolean;
            resume_text?: string | null;
        };
        CrunchView: {
            enabled: boolean;
        };
        /**
         * @description Точность показаний провайдера. `estimated` приходит от CodexBar, когда он считает
         *     расход по локальным журналам, а не по данным провайдера.
         * @enum {string}
         */
        DataConfidence: "exact" | "estimated";
        /** @description Решение маршрута: выбранная пара и полный разбор кандидатов. */
        Decision: {
            considered: components["schemas"]["Considered"][];
            effort: string;
            /**
             * Format: int64
             * @description Ожидаемая длительность задачи, по которой считался прогноз лимита.
             */
            expected_minutes?: number;
            harness: string;
            level?: string | null;
            model: string;
            nudge: unknown;
            /** Format: int64 */
            policy_revision: number;
            preset?: string | null;
            task_type: string;
            /** Format: int64 */
            watchdog_minutes?: number | null;
        };
        DecisionAnswer: string | string[];
        /** @enum {string} */
        DecisionBehavior: "allow" | "deny";
        DecisionBody: {
            behavior: components["schemas"]["DecisionBehavior"];
            /** @enum {string} */
            kind: "permission";
            message?: string | null;
        } | {
            answers: {
                [key: string]: components["schemas"]["DecisionAnswer"];
            };
            /** @enum {string} */
            kind: "question";
        };
        DecisionOutcomeView: {
            decision: components["schemas"]["DecisionBody"];
            /** Format: uuid */
            request_id: string;
            state: string;
            /** Format: uuid */
            winner_decision_id: string;
        };
        DefaultsView: {
            crunch_resume_text: string;
        };
        DelegationNode: {
            /** Format: uuid */
            block_id: string;
            /** Format: date-time */
            finished_at?: string | null;
            /** Format: uuid */
            parent_block_id?: string | null;
            /** Format: date-time */
            started_at: string;
            state: string;
            subagent?: components["schemas"]["SubagentRef"] | null;
            task?: string | null;
            tool_name?: string | null;
        };
        DiskView: {
            /** Format: int64 */
            free_bytes: number;
            label: string;
            /** Format: int64 */
            total_bytes: number;
        };
        DraftView: {
            attachment_ids: string[];
            /** Format: int64 */
            revision: number;
            text: string;
            updated_at?: string | null;
        };
        EditInputBody: {
            attachment_ids?: string[];
            /** Format: int64 */
            expected_revision: number;
            text: string;
        };
        EditInputOutcome: {
            /** Format: uuid */
            input_id: string;
            /** Format: int64 */
            revision: number;
        };
        EffectiveSettings: {
            effort: string;
            instruction_preset: components["schemas"]["AppliedPreset"];
            instruction_profile: string;
            model: string;
            permission_mode: string;
        };
        ErrorBody: {
            code: components["schemas"]["ErrorCode"];
            /** Format: uuid */
            correlation_id: string;
            details?: components["schemas"]["ErrorDetails"];
            message: string;
            retry: components["schemas"]["Retry"];
            scope: components["schemas"]["ErrorScope"];
        };
        /** @enum {string} */
        ErrorCode: "unauthenticated" | "invalid_credentials" | "too_many_attempts" | "session_revoked" | "session_expired" | "reauth_required" | "wrong_password" | "qr_expired" | "qr_approvals_exhausted" | "passkey_rejected" | "no_passkeys" | "passkeys_unavailable" | "origin_rejected" | "invalid_request" | "invalid_offset" | "not_found" | "method_not_allowed" | "idempotency_conflict" | "decision_conflict" | "request_not_open" | "decision_invalid" | "resource_limit" | "not_implemented" | "too_many_requests" | "db_unavailable" | "internal" | "queue_full" | "conversation_closed" | "execution_already_live" | "invalid_state" | "not_pending" | "schedule_time_out_of_range" | "too_many_scheduled" | "machine_not_found" | "machine_unavailable" | "capability_missing" | "limits_mismatch" | "harness_unknown" | "model_unknown" | "effort_not_allowed" | "permission_mode_unknown" | "settings_midsession_unsupported" | "profile_unknown" | "workdir_not_allowed" | "resume_harness_unsupported" | "group_not_found" | "group_name_taken" | "archive_active" | "conversation_deleted" | "conversation_live" | "attachment_not_ready" | "attachment_not_uploadable" | "attachment_unsupported" | "attachment_unavailable" | "offset_mismatch" | "upload_in_progress" | "checksum_mismatch" | "attachment_conflict" | "preset_not_found" | "preset_builtin" | "preset_revision_conflict" | "preset_title_taken" | "preset_limit_reached" | "preset_invalid" | "spawn_whitelist_invalid" | "limit_settings_invalid" | "routing_policy_invalid" | "routing_policy_conflict" | "task_type_unknown" | "no_executor_available" | "not_delegated" | "worktree_removed" | "worktree_not_removed";
        /** @description Детали ответа владельцу; содержимое решения не должно попадать в журналы ошибок. */
        ErrorDetails: {
            /** @description Разбор кандидатов маршрута (`no_executor_available`): не текст в `reason`. */
            candidates?: components["schemas"]["Considered"][];
            /**
             * Format: int64
             * @description Текущая версия пресета (`preset_revision_conflict`).
             */
            current_revision?: number;
            current_state?: string;
            /** @description Поле запроса, которое не прошло проверку (`preset_invalid`). */
            field?: string;
            /**
             * Format: date-time
             * @description Ближайший сброс лимита провайдера (`no_executor_available`).
             */
            nearest_reset?: string;
            question_id?: string;
            reason?: components["schemas"]["ErrorReason"];
            reasons?: components["schemas"]["ArchiveReason"][];
            /**
             * Format: int64
             * @description Через сколько секунд повторить попытку входа (`too_many_attempts`).
             */
            retry_after_seconds?: number;
            /** @description Отказ «Запустить сейчас» (`spawn_refused`): код, сообщение и состояние элемента. */
            spawn_refusal?: components["schemas"]["SpawnRefusal"];
            winner_decision?: components["schemas"]["DecisionBody"];
        };
        ErrorEnvelope: {
            error: components["schemas"]["ErrorBody"];
        };
        /** @enum {string} */
        ErrorReason: "closed_without_decision" | "generation_advanced" | "invalid_idempotency_key" | "input_not_queued" | "no_running_turn" | "request_open" | "compacting" | "unsupported_harness" | "capability_missing" | "revision_conflict" | "steer_in_flight" | "empty_content" | "content_too_long" | "attachment_unavailable" | "attachment_not_ready" | "attachment_not_found" | "run_in_progress" | "no_live_execution" | "conversation_closed" | "nul_character" | "spawn_refused" | "logout" | "owner" | "others" | "password_changed" | "idle" | "server" | "limit" | "cascade";
        /** @enum {string} */
        ErrorScope: "browser" | "auth" | "hub_db" | "delivery" | "worker_disk" | "cli_start" | "adapter_protocol" | "agent_service";
        Evidence: {
            /** Format: int32 */
            background_work?: number | null;
            /** @enum {string} */
            kind: "idle";
            /** Format: date-time */
            last_turn_at?: string | null;
            /**
             * @description Жив ли процесс CLI, пока сессия ждёт сообщения: `false` — исполнения нет или
             *     процесс умер, `true` — свежая телеметрия машины видит его живым. Без телеметрии
             *     (старый воркер, устаревшие метрики) поле не пишется: клиент судит по-старому.
             */
            process_alive?: boolean | null;
        } | {
            /** @enum {string} */
            kind: "working";
            lease_wait?: components["schemas"]["LeaseWait"] | null;
            phase: string;
            /** Format: date-time */
            phase_since?: string | null;
            summary?: string | null;
            tool_name?: string | null;
        } | {
            /** @enum {string} */
            kind: "waiting_for_user";
            phase: string;
            question?: string | null;
        } | {
            cpu_busy?: boolean | null;
            /** @enum {string} */
            kind: "stalled";
            /** Format: date-time */
            last_event_at: string;
            link: string;
            phase: string;
            process_alive?: boolean | null;
        } | {
            /** @enum {string} */
            kind: "quota_paused";
            provider?: string | null;
            reason: string;
            /** Format: date-time */
            until?: string | null;
            window?: string | null;
        } | {
            /** @enum {string} */
            kind: "failed";
            reason: string;
        } | {
            /** @enum {string} */
            kind: "stopped";
            outcome: string;
        };
        /** @description Файл, который агент отдал владельцу, в моделях чтения блока. */
        FileView: {
            /** Format: uuid */
            attachment_id: string;
            /** Format: int32 */
            height?: number | null;
            mime: string;
            name: string;
            note: string;
            /** Format: int64 */
            size: number;
            title: string;
            /** Format: int32 */
            width?: number | null;
        };
        /** @description Прогноз окна: средний и недавний темп, ожидаемое исчерпание. */
        ForecastView: {
            /** @description Исчерпание наступит раньше сброса окна. */
            before_reset: boolean;
            /** @description Когда окно кончится при среднем темпе; `null`, если темп нулевой. */
            exhausts_at?: string | null;
            /**
             * Format: int64
             * @description Средний темп по приросту расхода между снимками окна, промилле в час.
             */
            rate_permille_per_hour: number;
            /**
             * Format: int64
             * @description Темп за последний час (пятичасовое окно) или шесть часов (неделя, месяц).
             */
            recent_rate_permille_per_hour?: number | null;
        };
        FreshView: {
            /** Format: date-time */
            fresh_until: string;
        };
        Group: {
            created_at: string;
            icon?: components["schemas"]["GroupIcon"] | null;
            /** Format: uuid */
            id: string;
            name: string;
            /** Format: int32 */
            position: number;
        };
        GroupBody: {
            icon?: components["schemas"]["GroupIcon"] | null;
            name: string;
        };
        GroupCount: {
            /** Format: int64 */
            count: number;
            /** Format: uuid */
            id: string;
        };
        /**
         * @description Значок группы из постоянного набора; тот же список проверяет база.
         * @enum {string}
         */
        GroupIcon: "folder" | "briefcase" | "home" | "star" | "code" | "rocket" | "book" | "flask" | "wrench" | "server" | "globe" | "heart" | "user" | "users" | "gamepad" | "music";
        GroupPatch: {
            icon?: components["schemas"]["GroupIcon"] | null;
            name?: string | null;
            /** Format: int32 */
            position?: number | null;
        };
        /** @description Закрепление сессии внутри одной группы; глобальное `pinned` живёт отдельно. */
        GroupPinView: {
            /** Format: uuid */
            group_id: string;
            pinned_at: string;
        };
        GroupsBody: {
            group_ids: string[];
        };
        /** @description Вердикт стража: измерения, нарушившие пороги, и строка для `detail`. */
        GuardVerdict: {
            /** @description Хотя бы один порог нарушен (старение отменяет блокировку). */
            blocked: boolean;
            /** @description Одна английская строка для ответа и элемента очереди. */
            detail?: string | null;
            /** @description Метрики старше `metrics_max_age`: страж не включается. */
            stale: boolean;
            /** @description Человеческие имена нарушенных порогов (пусто — машина свободна). */
            violations: string[];
        };
        HarnessRef: {
            id: string;
            provider: string;
            title: string;
        };
        HarnessView: {
            id: string;
            images: boolean;
            models: components["schemas"]["ModelView"][];
            permission_modes: string[];
            provider: string;
            settings_midsession: boolean;
            settings_midsession_effort: boolean;
            title: string;
        };
        History: {
            series: components["schemas"]["ProviderSeries"][];
            /** @description Медианы скорости ответа по моделям; Codex не передаёт времени генерации и сюда не попадает. */
            speed: components["schemas"]["ModelSpeedSummary"][];
            usage: components["schemas"]["UsageDay"][];
        };
        HistoryPage: {
            items: components["schemas"]["TimelineItem"][];
            next_after?: string | null;
            next_before?: string | null;
        };
        /**
         * @description Найденный ответ или сообщение. `seq` — место в ленте (`created_seq` блока, `feed_seq`
         *     ввода); `jump_seq` — место верхнего предка вложенного блока субагента, иначе `seq`.
         */
        Hit: {
            /** @description Сессия в архиве (или принятый делегат до уборки). */
            archived: boolean;
            /** Format: uuid */
            block_id: string | null;
            /** Format: uuid */
            conversation_id: string;
            conversation_title: string;
            created_at: string;
            harness: string;
            /** Format: uuid */
            input_id: string | null;
            /** Format: int64 */
            jump_seq: number;
            kind: components["schemas"]["HitKind"];
            model: string;
            origin_kind: components["schemas"]["OriginKind"] | null;
            /** Format: int64 */
            seq: number;
            snippet: components["schemas"]["SnippetSegment"][];
        };
        /** @enum {string} */
        HitKind: "answer" | "message";
        HoldView: {
            /** @enum {string} */
            reason: "run_in_progress";
        } | {
            /** @enum {string} */
            reason: "awaiting_user_request";
        } | {
            /** @enum {string} */
            reason: "context_maintenance";
        } | {
            /** @enum {string} */
            reason: "no_live_execution";
        } | {
            /** @enum {string} */
            reason: "execution_starting";
        } | {
            /** @enum {string} */
            reason: "storage_low";
        } | {
            /** Format: uuid */
            conflict_conversation_id: string;
            /** @enum {string} */
            reason: "workdir_busy";
        } | {
            /** Format: uuid */
            input_id: string;
            machine_reason?: string | null;
            /** @enum {string} */
            reason: "attachments_pending";
        } | {
            provider: string;
            /** @enum {string} */
            reason: "quota_exhausted";
            /** Format: date-time */
            wake_at: string;
            window: string;
        } | {
            provider: string;
            /** @enum {string} */
            reason: "weekly_reserve";
            /** Format: date-time */
            resets_at?: string | null;
        } | {
            /** @enum {string} */
            reason: "machine_not_reconciled";
        } | {
            /** Format: uuid */
            input_id: string;
            /** @enum {string} */
            reason: "capability_missing";
        };
        HotspotView: {
            /** Format: int64 */
            bytes: number;
            /**
             * Format: uuid
             * @description Рабочая копия беседы; пусто — каталог владельца.
             */
            conversation_id?: string | null;
            /** Format: int64 */
            incremental_bytes: number;
            /** @description `target`, `node_modules`, `svelte_kit` или `web_build`. */
            kind: string;
            measured_at?: string | null;
            /** @description Последняя запись сборки. */
            newest_at?: string | null;
            path: string;
            title?: string | null;
        };
        /** @description Коммит из итога сессии: короткий хеш и первая строка сообщения. */
        HubCommit: {
            sha: string;
            title: string;
        };
        /**
         * @description Факты события хаба. Текст для модели остаётся прежним, а здесь лежит то, что
         *     владелец видит в ленте.
         */
        HubEvent: {
            /**
             * Format: uuid
             * @description `alarm`: сработавший будильник.
             */
            alarm_id?: string | null;
            alarm_key?: string | null;
            /** @description `alarm` с условием `file`: путь и наблюдённый размер файла. */
            alarm_path?: string | null;
            /** Format: int64 */
            alarm_size?: number | null;
            /** @description Пачка сработавших будильников: по объекту на каждый. */
            alarms?: components["schemas"]["AlarmEvent"][] | null;
            answer?: string | null;
            /** Format: uuid */
            answer_block_id?: string | null;
            /** Format: int64 */
            answer_bytes?: number | null;
            /**
             * Format: date-time
             * @description `alarm`: когда будильник взвели.
             */
            armed_at?: string | null;
            /** @description Ответ кончается вопросом в тексте, а не карточкой. */
            asks_in_text?: boolean | null;
            branch?: string | null;
            branch_kept?: boolean | null;
            /** Format: int64 */
            changed_files?: number | null;
            /** Format: uuid */
            child_id?: string | null;
            /** @description `accepted`: событие ушло другому подписчику, а не принявшему родителю. */
            closed?: boolean | null;
            commits?: components["schemas"]["HubCommit"][] | null;
            /** @description `alarm`: разобранное условие в именах полей `alarm_set`. */
            condition?: unknown;
            cpu_busy?: boolean | null;
            /** @description `approval`: `allowed`, `denied`, `failed` или `lapsed`. */
            decision?: string | null;
            detail?: string | null;
            /** @description `machine_*` и `lease_long_hold`: числа и имена, из которых собран английский текст. */
            details?: unknown;
            /** Format: int64 */
            duration_ms?: number | null;
            fired_reason?: string | null;
            /**
             * @description `session_result`, `asked`, `stalled`, `failed`, `quota_paused`, `stopped`,
             *     `accepted`, `approval` или `deleted`.
             */
            kind: string;
            /** Format: date-time */
            last_event_at?: string | null;
            /**
             * Format: uuid
             * @description `machine_*`: машина, о которой говорит предупреждение.
             */
            machine_id?: string | null;
            /** @description `machine_*`: имя машины для заголовка в ленте. */
            machine_name?: string | null;
            /** @description `alarm`: заметка себе, доставленная вместе с событием. */
            note?: string | null;
            /** @description `true`: толчки кончились без маркера, итог отпущен правилом. */
            nudge_exhausted?: boolean | null;
            /**
             * Format: int64
             * @description Сколько автоматических толчков получила цель до этого итога.
             */
            nudges?: number | null;
            outcome?: string | null;
            phase?: string | null;
            process_alive?: boolean | null;
            provider?: string | null;
            question?: string | null;
            reason?: string | null;
            /** Format: uuid */
            request_id?: string | null;
            /**
             * Format: date-time
             * @description `limit_resumed` и `quota_paused`: когда лимит сбросится или сбросился.
             */
            resets_at?: string | null;
            /** Format: uuid */
            session_id?: string | null;
            /** @description `alarm` с условием `session_state`: цели и их состояние на момент срабатывания. */
            sessions?: components["schemas"]["AlarmSession"][] | null;
            state?: string | null;
            /** Format: int64 */
            tokens_in?: number | null;
            /** Format: int64 */
            tokens_out?: number | null;
            /** Format: int64 */
            turns?: number | null;
            /** Format: date-time */
            until?: string | null;
            window?: string | null;
            /**
             * Format: int64
             * @description Сумма ходов без ожидания владельца: разрешения и вопросы внутри хода.
             */
            working_ms?: number | null;
            /** @description `accepted`: `removed`, `none`, `dirty` или `failed`. */
            worktree?: string | null;
        };
        InputContentBlock: {
            text: string;
            /** @enum {string} */
            type: "text";
        } | (components["schemas"]["AttachmentView"] & {
            /** @enum {string} */
            type: "attachment";
        });
        /**
         * @description Форма `inputs.origin` для клиентов. После удаления отправителя остаются только
         *     `kind`, `chain_depth` и `sender_deleted`.
         */
        InputOrigin: {
            /** Format: int32 */
            chain_depth: number;
            event?: components["schemas"]["HubEvent"] | null;
            /** Format: uuid */
            from?: string | null;
            harness?: string | null;
            kind: string;
            model?: string | null;
            sender_deleted?: boolean | null;
            sha256?: string | null;
            title?: string | null;
        };
        /** @enum {string} */
        InputState: "queued" | "dispatched" | "on_machine" | "transmitting" | "accepted" | "acceptance_unconfirmed" | "cancelled" | "rejected";
        InputView: {
            attachments: components["schemas"]["AttachmentView"][];
            content: components["schemas"]["InputContentBlock"][];
            created_at: string;
            created_seq: string;
            feed_seq?: string | null;
            /** Format: uuid */
            id: string;
            idempotency_key: string;
            origin?: components["schemas"]["InputOrigin"] | null;
            /** Format: uuid */
            retry_of?: string | null;
            /**
             * Format: int64
             * @description Ревизия текста и вложений: правка ожидающего ввода идёт сравнением-и-заменой.
             */
            revision: number;
            state: string;
        };
        /** @enum {string} */
        JobState: "pending" | "fired" | "cancelled" | "failed";
        /** @description Страница ленты одного субагента: корень (вызов) и его потомки по любой глубине. */
        LanePage: {
            /** @description Потомки корня по возрастанию `created_seq`; сам корень сюда не входит. */
            items: components["schemas"]["BlockView"][];
            next_before?: string | null;
            root: components["schemas"]["BlockView"];
        };
        LeakedView: {
            /** Format: int64 */
            age_s: number;
            comm: string;
            /** Format: uuid */
            conversation_id?: string | null;
            /** Format: uuid */
            execution_id: string;
            /** Format: int32 */
            pid: number;
            /** Format: int64 */
            rss_bytes: number;
            title?: string | null;
        };
        /** @description Запись очереди. Времена — в часах хаба. */
        LeaseEntryView: {
            /**
             * Format: uuid
             * @description Сессия исполнения, если хаб знает его на этой машине.
             */
            conversation_id?: string | null;
            /** @description Имя рабочего каталога клиента. */
            dir: string;
            /** Format: uuid */
            execution_id?: string | null;
            holder: components["schemas"]["LeaseHolder"];
            kind: components["schemas"]["LeaseKind"];
            label: string;
            lease_id: string;
            /**
             * Format: int32
             * @description Место в очереди с единицы; `null` у держателей.
             */
            position?: number | null;
            requested_at?: string | null;
            /** @description Время выдачи; `null` у ожидающих. */
            since?: string | null;
            /** Format: int32 */
            weight: number;
        };
        /** @enum {string} */
        LeaseHolder: "execution" | "owner";
        /** @enum {string} */
        LeaseKind: "gate" | "build";
        /** @description Последнее полное состояние очереди машины. */
        LeaseStateView: {
            holders: components["schemas"]["LeaseEntryView"][];
            /** @description Время снимка на машине, в часах хаба. */
            observed_at: string;
            /** Format: int32 */
            slots: number;
            waiters: components["schemas"]["LeaseEntryView"][];
        };
        /** @description Ожидание исполнения в очереди: свидетельство активности `lease_wait`. */
        LeaseWait: {
            kind: components["schemas"]["LeaseKind"];
            /** Format: int32 */
            position: number;
            since: string;
        };
        LimitSettings: {
            /**
             * Format: int32
             * @description Красный цвет, когда остаток меньше этого процента.
             */
            danger_remaining_pct: number;
            /**
             * Format: int32
             * @description Сколько недельного остатка оставить себе; `0` — без резерва.
             */
            reserve_remaining_pct: number;
            /**
             * Format: int32
             * @description Жёлтый цвет, когда остаток меньше этого процента.
             */
            warn_remaining_pct: number;
        };
        LimitSettingsBody: {
            settings: components["schemas"]["LimitSettings"] | null;
        };
        LimitSettingsView: {
            default_settings: components["schemas"]["LimitSettings"];
            is_default: boolean;
            settings: components["schemas"]["LimitSettings"];
        };
        LimitThresholdsView: {
            /**
             * Format: int32
             * @description Красный, когда остаток меньше этого процента.
             */
            danger_remaining_pct: number;
            /**
             * Format: int32
             * @description Сколько недельного остатка владелец оставляет себе; `0` — без резерва.
             */
            reserve_remaining_pct: number;
            /** Format: int64 */
            stale_after_secs: number;
            /** @description Устарело: раньше первый порог окрашивал полосу. Оставлено для старых клиентов. */
            warn_pcts: number[];
            /**
             * Format: int32
             * @description Жёлтый, когда остаток меньше этого процента.
             */
            warn_remaining_pct: number;
            /**
             * Format: int32
             * @description Действующий резерв как расход, %: совместимо с прежним полем (`100 - reserve`).
             */
            weekly_reserve_pct: number;
        };
        LimitsView: {
            /** Format: int64 */
            max_attachment_bytes: number;
            /** Format: int64 */
            max_attachments_per_input: number;
            /** Format: int64 */
            max_input_meta_bytes: number;
            /** Format: int64 */
            max_input_text_bytes: number;
            /** Format: int64 */
            queue_limit: number;
            /** Format: int64 */
            upload_chunk_bytes: number;
        };
        ListItem: components["schemas"]["AttributeView"] & {
            activity: components["schemas"]["ActivityView"];
            crunch: components["schemas"]["CrunchView"];
            effort: string;
            last_activity_at?: string | null;
            model: string;
            /**
             * @description Конкретная модель за псевдонимом `model` (`fable[1m]` → `claude-fable-5-2`): из ходов
             *     сессии, до первого хода на этой модели — последнее наблюдение на машине; `None` — неизвестно.
             */
            model_resolved?: string | null;
            permission_mode: string;
            /**
             * @description Начало последнего ответа агента без разметки, до 160 символов: финальный текст
             *     основного агента в последнем завершённом ходе; `None` — ответа ещё нет.
             */
            preview?: string | null;
            queue: components["schemas"]["QueueView"];
            /** @description Решение маршрута, выбравшее исполнителя; `None` — запуск без маршрута. */
            routing?: unknown;
            /** Format: int32 */
            unread_count: number;
            /**
             * Format: int64
             * @description Сумма ходов без ожидания владельца; у живой сессии — вместе с текущим ходом.
             */
            working_ms: number;
        };
        ListPage: {
            counts: components["schemas"]["Counts"];
            items: components["schemas"]["ListItem"][];
            next_cursor?: string | null;
        };
        ListSnapshot: {
            counts: components["schemas"]["Counts"];
            groups: components["schemas"]["Group"][];
            items: components["schemas"]["ListItem"][];
            limits: components["schemas"]["ProviderView"][];
            /**
             * @description Задачи досок плана корневых оркестраторов вне архива и корзины; готовые и
             *     отменённые — за 7 дней, не больше 50 оркестраторов. Снимок заменяет набор целиком.
             */
            plan_tasks: components["schemas"]["PlanTask"][];
            presets: components["schemas"]["PresetSummary"][];
            truncated: boolean;
        };
        LiveExecutionView: {
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            execution_id: string;
            /** Format: int64 */
            generation: number;
            native_conversation_id?: string | null;
            outcome?: string | null;
            state: string;
        };
        /**
         * @description Запас машины для предпросмотра: минимум свободного диска, нагрузка на ядро,
         *     свободная память и свежесть метрик.
         */
        MachineHeadroom: {
            /** Format: double */
            free_disk_gb?: number | null;
            /** Format: double */
            load_per_core?: number | null;
            /** Format: double */
            mem_available_gb?: number | null;
            /** Format: date-time */
            observed_at?: string | null;
            stale: boolean;
        };
        MachineRef: {
            /** Format: uuid */
            id: string;
            name: string;
        };
        MachineView: {
            allowed_roots: string[];
            availability: string;
            availability_since?: string | null;
            capabilities: string[];
            containment?: string | null;
            harnesses: components["schemas"]["HarnessView"][];
            /** Format: int64 */
            heartbeat_age_ms?: number | null;
            home_dir?: string | null;
            /** Format: uuid */
            id: string;
            lease?: components["schemas"]["LeaseStateView"] | null;
            limits_ok: boolean;
            metrics?: components["schemas"]["MetricsView"] | null;
            name: string;
            os?: string | null;
            sessions: components["schemas"]["SessionView"][];
            warnings: components["schemas"]["MachineWarning"][];
            watch?: components["schemas"]["WatchView"] | null;
            worker_version?: string | null;
        };
        /** @enum {string} */
        MachineWarning: "disk_usage_high" | "metrics_stale" | "limits_mismatch" | "cj_journal_stopped" | "sj_journal_stopped" | "memory_low" | "processes_leaked";
        MachinesSnapshot: {
            items: components["schemas"]["MachineView"][];
        };
        MachinesView: {
            /** Format: int64 */
            disk_critical_free_gb: number;
            /** Format: int64 */
            disk_warn_free_gb: number;
            /**
             * Format: int32
             * @description Устарело: прежний порог по доле занятого места; клиент читает свободные гигабайты.
             */
            disk_warn_pct: number;
            /** Format: int64 */
            metrics_stale_after_secs: number;
        };
        MeView: {
            login: string;
            /** Format: uuid */
            owner_id: string;
            /**
             * Format: uuid
             * @description Сеанс браузера, которым пришёл запрос.
             */
            session_id: string;
        };
        /** @description Картинка в моделях чтения блока. */
        MediaView: {
            /** Format: int32 */
            height?: number | null;
            /** Format: uuid */
            media_id: string;
            mime: string;
            /** Format: int64 */
            size: number;
            /** Format: int32 */
            width?: number | null;
        };
        MethodsView: {
            passkey: boolean;
            password: boolean;
        };
        MetricsView: {
            /** Format: int32 */
            cpu_cores: number;
            /** Format: int32 */
            cpu_permille: number;
            disks: components["schemas"]["DiskView"][];
            executions: components["schemas"]["ProcessView"][];
            /** Format: int32 */
            load15_centi: number;
            /** Format: int32 */
            load1_centi: number;
            /** Format: int32 */
            load5_centi: number;
            /** Format: int64 */
            machine_time_ms: number;
            /** Format: int64 */
            mem_available_bytes: number;
            /** Format: int64 */
            mem_total_bytes: number;
            /** Format: date-time */
            observed_at: string;
        };
        ModelSpeed: {
            /**
             * Format: int64
             * @description Медиана по запросам хода: от начала запроса до первого кадра ответа.
             */
            first_token_ms?: number | null;
            /**
             * Format: int64
             * @description Сумма времени генерации по запросам хода; нет, если его нельзя выделить честно.
             */
            generation_ms?: number | null;
            model: string;
            /** Format: int64 */
            output_tokens: number;
            /** Format: double */
            tokens_per_second?: number | null;
        };
        ModelSpeedSummary: {
            model: string;
            /** @description Весь запрошенный диапазон. */
            period: components["schemas"]["SpeedStats"];
            provider: string;
            /** @description Последний день диапазона в часовом поясе запроса. */
            today: components["schemas"]["SpeedStats"];
        };
        ModelUsageView: {
            /** Format: int64 */
            cache_read_tokens: number;
            /** Format: int64 */
            cache_write_tokens: number;
            /** Format: int64 */
            input_tokens: number;
            model: string;
            /** Format: int64 */
            output_tokens: number;
        };
        ModelView: {
            /**
             * Format: int64
             * @description Окно контекста модели в токенах, если харнесс его знает.
             */
            context_window?: number | null;
            efforts: string[];
            id: string;
            /** @description Модель принимает картинки; `null`: у харнесса это не свойство модели. */
            images?: boolean | null;
            resolved?: components["schemas"]["ResolutionView"] | null;
            title: string;
        };
        NewAttachment: {
            /** Format: uuid */
            conversation_id: string;
            mime?: string | null;
            name: string;
            sha256: string;
            /** Format: int64 */
            size: number;
        };
        /** @description Событие автопинка для `session_inspect` и PWA. */
        NudgeEventView: {
            at: string;
            /** Format: int32 */
            attempt: number;
            /** Format: uuid */
            id: string;
            outcome: string;
            signals: unknown;
        };
        NudgeEvents: {
            events: components["schemas"]["NudgeEventView"][];
        };
        /** @description Действующие значения правила: ответ `nudge_set` и снимок разговора. */
        NudgeRuleView: {
            /** Format: int32 */
            background_minutes: number;
            /** Format: int32 */
            consecutive: number;
            /** Format: int32 */
            context_warn_percent?: number | null;
            done_markers: string[];
            enabled: boolean;
            held: boolean;
            /** Format: int32 */
            idle_minutes: number;
            /** Format: int32 */
            max_consecutive: number;
            /** Format: int32 */
            max_total: number;
            /** Format: uuid */
            notify_session_id?: string | null;
            /** @description Файл отчёта: путь и рост считаются прогрессом. */
            report_file?: string | null;
            /** Format: uuid */
            session_id: string;
            set_by: string;
            /** Format: uuid */
            set_by_session?: string | null;
            state: string;
            /** Format: int32 */
            total: number;
        };
        NudgeState: {
            events: components["schemas"]["NudgeEventView"][];
            rule?: components["schemas"]["NudgeRuleView"] | null;
        };
        /**
         * @description Кто написал найденное сообщение: владелец, другая сессия, бриф запуска или итог делегата.
         * @enum {string}
         */
        OriginKind: "owner" | "message" | "spawn" | "result";
        OwnerFileContent: {
            content: string;
            files: components["schemas"]["OwnerMemoryFileView"][];
            harness: string;
            kind: string;
            mtime?: string | null;
            path: string;
            present: boolean;
            /** @description Почему файл не показан при `present = false`: пусто или `symlink`. */
            reason: string;
            /** Format: int64 */
            size: number;
            truncated: boolean;
        };
        OwnerFileView: {
            harness: string;
            kind: string;
            mtime?: string | null;
            path: string;
            present: boolean;
            /** @description Почему файл не показан при `present = false`: пусто или `symlink`. */
            reason: string;
            /** Format: int64 */
            size: number;
        };
        /** @description Одно сообщение владельца для списка перехода по своим сообщениям. */
        OwnerInputView: {
            /** Format: int64 */
            attachments_count: number;
            created_at: string;
            /** Format: uuid */
            id: string;
            origin?: components["schemas"]["InputOrigin"] | null;
            /** @description Первая часть текста без разбора: строку списка выбирает клиент. */
            preview: string;
            /** @description Место ввода в ленте (`feed_seq`): по нему клиент прыгает к сообщению. */
            seq: string;
        };
        OwnerInputsPage: {
            items: components["schemas"]["OwnerInputView"][];
            next_before?: string | null;
        };
        OwnerMemoryFileView: {
            mtime?: string | null;
            name: string;
            /** Format: int64 */
            size: number;
        };
        OwnerMemoryProjectView: {
            /** Format: int64 */
            files: number;
            has_index: boolean;
            id: string;
        };
        OwnerOverviewView: {
            files: components["schemas"]["OwnerFileView"][];
            memory_projects: components["schemas"]["OwnerMemoryProjectView"][];
        };
        OwnerSessionView: {
            /** @description Живые сеансы, вход которых подтвердил этот. */
            approved: string[];
            /**
             * Format: uuid
             * @description Сеанс, подтвердивший вход по QR.
             */
            approved_by?: string | null;
            /** Format: date-time */
            created_at: string;
            /** @description `null` — адрес неизвестен («IP неизвестен»). */
            created_ip?: string | null;
            current: boolean;
            /** Format: uuid */
            id: string;
            /** @description Подпись устройства по user agent и подсказкам клиента. */
            label?: string | null;
            last_ip?: string | null;
            /** Format: date-time */
            last_seen_at: string;
            location?: components["schemas"]["SessionLocationView"] | null;
            method: components["schemas"]["SignInMethod"];
            /** @description Имя от владельца. */
            name?: string | null;
            /** @description Сырой user agent для подробностей; заявлен клиентом. */
            user_agent?: string | null;
        };
        OwnerSessionsView: {
            /** Format: uuid */
            current_id: string;
            /**
             * Format: date-time
             * @description До какого момента текущий сеанс свеж (действия управления без повторного входа).
             */
            fresh_until?: string | null;
            /** Format: int32 */
            idle_timeout_days: number;
            /** @description Живые сеансы: текущий первым, затем по последней активности. */
            sessions: components["schemas"]["OwnerSessionView"][];
        };
        /**
         * @description Правка правила владельцем из PWA: агентских ограничений на потомков нет,
         *     владелец ставит правило на любую свою сессию.
         */
        OwnerUpdate: {
            /** Format: int32 */
            context_warn_percent?: number | null;
            done_markers?: string[] | null;
            enabled?: boolean | null;
            /** Format: int32 */
            idle_minutes?: number | null;
            /** Format: int32 */
            max_nudges?: number | null;
            paused?: boolean | null;
            text_preset?: string | null;
        };
        PartView: {
            agent: string;
            /** Format: uuid */
            block_id: string;
            kind: string;
            model: string;
        };
        PasskeyChallengeView: {
            /**
             * Format: uuid
             * @description Обряд, к которому относится ответ ключа; живёт 5 минут, используется один раз.
             */
            challenge_id: string;
            /** @description `{publicKey: …}` для `navigator.credentials.create()` или `get()`. */
            options: Record<string, never>;
        };
        PasskeyReauthFinish: {
            /** Format: uuid */
            challenge_id: string;
            credential: Record<string, never>;
        };
        PasskeyRegisterFinish: {
            /** Format: uuid */
            challenge_id: string;
            credential: Record<string, never>;
            /** @description До 64 символов; без имени — подпись устройства, на котором ключ добавлен. */
            name?: string | null;
        };
        PasskeySignIn: {
            /** Format: uuid */
            challenge_id: string;
            /** @description `PublicKeyCredential.toJSON()`. */
            credential: Record<string, never>;
            /** @description Подсказки клиента для подписи устройства. */
            device?: unknown;
        };
        PasskeyStartBody: Record<string, never>;
        PasskeyView: {
            /** @description Ключ синхронизируется между устройствами (менеджер паролей, iCloud, Google). */
            backed_up: boolean;
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            last_used_at?: string | null;
            name: string;
        };
        PasskeysView: {
            passkeys: components["schemas"]["PasskeyView"][];
        };
        PasswordSignIn: {
            /** @description Подсказки клиента для подписи устройства: только для показа, не больше 2 КиБ. */
            device?: unknown;
            login: string;
            password: string;
        };
        PatchScheduled: {
            content?: components["schemas"]["ContentBlock"][] | null;
            /** Format: date-time */
            run_at?: string | null;
        };
        PauseReason: {
            /** @enum {string} */
            reason: "user_stop";
        } | {
            /** @enum {string} */
            reason: "fatal_error";
        } | {
            /** @enum {string} */
            reason: "process_exited";
        } | {
            provider?: string | null;
            /** @enum {string} */
            reason: "limit_reached";
            /** Format: date-time */
            resets_at?: string | null;
            window?: string | null;
        } | {
            /** Format: uuid */
            input_id: string;
            /** @enum {string} */
            reason: "unconfirmed_delivery";
        } | {
            /** @enum {string} */
            reason: "owner_lost";
        } | {
            /** @enum {string} */
            reason: "restore_pending";
        } | {
            compaction?: components["schemas"]["CompactionProgress"] | null;
            /** @enum {string} */
            reason: "session_stuck";
        };
        PlanCommit: {
            sha: string;
            subject: string;
        };
        PlanList: {
            /** @description Задачи по номерам с выведенными полями исполнителя. */
            tasks: components["schemas"]["PlanTaskDetail"][];
        };
        /**
         * @description Состояние задачи плана.
         * @enum {string}
         */
        PlanState: "waiting" | "in_progress" | "in_review" | "done" | "cancelled" | "blocked";
        /**
         * @description Задача плана: только хранимые поля. Так она идёт в снимок владельца и в
         *     `plan_task_changed`; исполнитель известен клиенту из списка разговоров.
         */
        PlanTask: {
            /** Format: int32 */
            attempts: number;
            blocker: string | null;
            created_at: string;
            /** Format: uuid */
            executor_id: string | null;
            findings: string[];
            /** Format: uuid */
            id: string;
            note: string | null;
            /** Format: int32 */
            number: number;
            /** Format: uuid */
            orchestrator_id: string;
            /** Format: int64 */
            revision: number;
            state: components["schemas"]["PlanState"];
            title: string;
            updated_at: string;
        };
        /**
         * @description Задача с выведенными полями исполнителя: считается по запросу
         *     (`GET /api/v1/plan` и `plan_list`), в события не идёт.
         */
        PlanTaskDetail: components["schemas"]["PlanTask"] & {
            /** @description Коммиты ходов исполнителя, свежие первыми, без повторов, не больше 50. */
            commits: components["schemas"]["PlanCommit"][];
            /** @description Список коммитов неполон: обрезан или git не смог прочитать диапазон. */
            commits_truncated: boolean;
            /** Format: int64 */
            files_changed: number;
            /**
             * Format: uuid
             * @description Последний ответ основного агента исполнителя.
             */
            report_block_id: string | null;
            /**
             * Format: int64
             * @description Место этого ответа в ленте: ссылка открывает ленту сразу вокруг него.
             */
            report_seq: number | null;
        };
        Point: {
            observed_at: string;
            /** Format: double */
            used_pct: number;
            /** Format: int32 */
            used_permille: number;
        };
        /** @description Политика маршрутизации владельца. `None` в базе — встроенная политика. */
        Policy: {
            /** @default {} */
            max_parallel: {
                [key: string]: number;
            };
            /** @default {} */
            providers: {
                [key: string]: components["schemas"]["ProviderThresholds"];
            };
            /** @default {} */
            task_types: {
                [key: string]: components["schemas"]["TaskType"];
            };
            /** @default allow */
            unknown_limits: components["schemas"]["UnknownLimits"];
            /** @default [] */
            unlimited_models: string[];
        };
        /** @description Детальный ответ: текст только у пользовательских пресетов. */
        Preset: components["schemas"]["PresetSummary"] & {
            text?: string | null;
        };
        PresetBody: {
            description?: string | null;
            purpose?: components["schemas"]["PresetPurpose"];
            text: string;
            title: string;
        };
        /**
         * @description Вид пресета в списках и событиях.
         * @enum {string}
         */
        PresetKind: "clean" | "owner" | "custom" | "nudge";
        /** @description Кто создал пресет или внёс последнюю правку; текст правки не несёт. */
        PresetOrigin: {
            at?: string | null;
            /** @description `builtin`, `owner` или `agent`. */
            by: string;
            /** Format: uuid */
            conversation_id?: string | null;
            conversation_title?: string | null;
        };
        PresetPatchBody: {
            /** Format: int64 */
            base_revision: number;
            description?: string | null;
            text?: string | null;
            title?: string | null;
        };
        /**
         * @description Назначение пресета: обычные инструкции агента или текст автопинка.
         * @enum {string}
         */
        PresetPurpose: "instructions" | "nudge";
        /** @description Строка списка, события и снимка: текста пресета здесь нет. */
        PresetSummary: {
            created: components["schemas"]["PresetOrigin"];
            description: string;
            /** @description `clean`, `owner`, `nudge_default` или UUID пользовательского пресета. */
            id: string;
            kind: components["schemas"]["PresetKind"];
            last_edit?: components["schemas"]["PresetOrigin"] | null;
            /** @description `instructions` или `nudge`. */
            purpose: components["schemas"]["PresetPurpose"];
            /** Format: int64 */
            revision?: number | null;
            /** Format: int64 */
            text_bytes?: number | null;
            title: string;
            /** Format: int64 */
            used_by: number;
        };
        /** @enum {string} */
        ProbeReason: "database" | "service";
        /** @enum {string} */
        ProbeStatus: "ok" | "not_ready";
        ProbeView: {
            reason?: components["schemas"]["ProbeReason"];
            services?: string[];
            status: components["schemas"]["ProbeStatus"];
        };
        ProcessView: {
            alive: boolean;
            /** Format: int32 */
            cpu_permille: number;
            /** Format: uuid */
            execution_id: string;
            /** Format: int64 */
            rss_bytes: number;
        };
        PromptSuggestionSettings: {
            /** @description Подсказки следующего сообщения в пустом поле ввода сессий Claude Code. */
            enabled: boolean;
        };
        PromptSuggestionView: {
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            execution_id: string;
            /** @description Предсказанное следующее сообщение владельца, не длиннее 2 КиБ. */
            text: string;
        };
        /** @description Незакрытый инцидент страницы статуса. */
        ProviderIncident: {
            name: string;
            /** @description Короткая ссылка на инцидент, если страница её дала. */
            url?: string | null;
        };
        ProviderSeries: {
            five_hour: components["schemas"]["Point"][];
            provider: string;
            weekly: components["schemas"]["Point"][];
            weekly_fable: components["schemas"]["Point"][];
        };
        /**
         * @description Уровень, важный для маршрута; порядок — от лучшего к худшему.
         * @enum {string}
         */
        ProviderStatusLevel: "ok" | "degraded" | "maintenance" | "partial_outage" | "major_outage" | "unknown";
        /** @description Статус провайдера для клиентов. */
        ProviderStatusView: {
            /** @description Когда уровень сменился в последний раз. */
            changed_at: string;
            /** @description Важные для харнесса компоненты не в рабочем состоянии. */
            components: string[];
            /** @description Общая строка страницы, например «Partial System Outage». */
            description: string;
            /** @description Последнее удачное чтение страницы. */
            fetched_at?: string | null;
            /** @description До трёх незакрытых инцидентов. */
            incidents: components["schemas"]["ProviderIncident"][];
            /** @description Уровень по компонентам, важным для харнесса; `unknown` — данных нет или они устарели. */
            level: components["schemas"]["ProviderStatusLevel"];
            /** @description Последние известные данные старше трёх периодов опроса. */
            stale: boolean;
            /** @description Страница статуса для человека. */
            url: string;
        };
        /** @description Пороги провайдера в процентах расхода. */
        ProviderThresholds: {
            /**
             * Format: double
             * @default null
             */
            max_five_hour_percent: number | null;
            /**
             * Format: double
             * @default null
             */
            max_monthly_percent: number | null;
            /**
             * Format: double
             * @description Недельная квота Fable у Claude; без значения действует `max_weekly_percent`.
             * @default null
             */
            max_weekly_fable_percent: number | null;
            /**
             * Format: double
             * @default null
             */
            max_weekly_percent: number | null;
        };
        ProviderView: {
            /** @description `estimated`, когда CodexBar вывел расход из локальных журналов, иначе `exact`. */
            data_confidence: components["schemas"]["DataConfidence"];
            five_hour?: components["schemas"]["WindowView"] | null;
            /** @description У провайдера нет пятичасового окна (например, план без этого лимита): счётчик скрывается. */
            five_hour_absent: boolean;
            monthly?: components["schemas"]["WindowView"] | null;
            provider: string;
            status?: components["schemas"]["ProviderStatusView"] | null;
            weekly?: components["schemas"]["WindowView"] | null;
            weekly_fable?: components["schemas"]["WindowView"] | null;
            weekly_reserve_active: boolean;
            /** @description Сброс недели, в окне которой включился резерв. */
            weekly_reserve_resets_at?: string | null;
        };
        PutDraft: {
            attachment_ids?: string[];
            /** Format: int64 */
            base_revision?: number;
            device_id?: string | null;
            text: string;
        };
        QrDecisionView: {
            state: components["schemas"]["QrState"];
        };
        QrPollBody: {
            after?: components["schemas"]["QrState"] | null;
            poll_token: string;
        };
        QrPollView: {
            session?: components["schemas"]["OwnerSessionView"] | null;
            /**
             * @description `expired` также значит «код не найден» и «сеанс уже выдан другому опросу»:
             *     устройство начинает заново.
             */
            state: components["schemas"]["QrState"];
        };
        /**
         * @description Карточка запроса на вход. Адрес, место и возраст кода видит хаб; устройство заявлено
         *     клиентом и показывается как «заявлено устройством: …».
         */
        QrRequestView: {
            /**
             * Format: int64
             * @description Сколько секунд назад код показан.
             */
            age_seconds: number;
            /** Format: date-time */
            created_at: string;
            /** Format: date-time */
            expires_at: string;
            /** @description `null` — адрес неизвестен. */
            ip?: string | null;
            /** @description Подпись по user agent и подсказкам клиента; заявлена клиентом. */
            label?: string | null;
            location?: components["schemas"]["SessionLocationView"] | null;
            /** @description Страна запроса известна и отличается от страны текущего сеанса. */
            other_country: boolean;
            user_agent?: string | null;
        };
        QrSecretBody: {
            /** @description Фрагмент ссылки из кода (после `#`). */
            secret: string;
        };
        QrStartBody: {
            /** @description Подсказки клиента для подписи устройства (как у входа по паролю). */
            device?: unknown;
        };
        QrStartView: {
            /** Format: date-time */
            expires_at: string;
            /** @description Токен опроса: только для `POST /qr/poll` этого устройства, в код не попадает. */
            poll_token: string;
            /** @description Содержимое QR: `{origin}/qr#<secret>`. */
            url: string;
        };
        /**
         * @description Состояние кода, как его видит опрашивающее устройство.
         * @enum {string}
         */
        QrState: "pending" | "scanned" | "approved" | "denied" | "expired";
        /** @description Элемент очереди для PWA, снимка и события. */
        QueueItemView: {
            /** Format: int32 */
            attempts: number;
            /** Format: uuid */
            child_id?: string | null;
            /** Format: date-time */
            created_at: string;
            detail: unknown;
            /** Format: date-time */
            expires_at: string;
            /** @description Пара прямого запуска; у маршрута появляется только после старта. */
            harness?: string | null;
            /** Format: uuid */
            id: string;
            level?: string | null;
            model?: string | null;
            /** Format: date-time */
            not_before: string;
            /** Format: uuid */
            parent_id: string;
            /** Format: int64 */
            position: number;
            reason: string;
            result?: unknown;
            state: string;
            /** @description Тип задачи маршрута; `null` у прямого запуска. */
            task_type?: string | null;
            /** @description Запрошенный заголовок будущей сессии; `null` — автоназвание. */
            title?: string | null;
            /** Format: date-time */
            updated_at: string;
        };
        QueueView: {
            hold_reasons: components["schemas"]["HoldView"][];
            pause_reasons: components["schemas"]["PauseReason"][];
            /** Format: int32 */
            queued: number;
        };
        RangeResponse: {
            events: components["schemas"]["WireEvent"][];
            head: string;
        } | {
            resync_required: components["schemas"]["ResyncRequired"];
        };
        ReadBody: {
            seq: string;
        };
        ReauthPassword: {
            password: string;
        };
        RecoverySignIn: {
            device?: unknown;
            token: string;
        };
        RefreshView: {
            /** @description Провайдеры, чьим машинам отправлена просьба об опросе. */
            requested: string[];
            /** Format: int64 */
            retry_after_secs: number;
        };
        RenamePasskey: {
            /** @description 1..64 символа. */
            name: string;
        };
        RenameSession: {
            /** @description До 64 символов; пустая строка снимает имя. */
            name: string;
        };
        /**
         * @description Вопрос или разрешение на том месте ленты, где агент его задал: открытый запрос
         *     стоит у поля ввода, решённый остаётся здесь и после перезагрузки.
         */
        RequestItemView: {
            created_at: string;
            created_seq: string;
            decision_payload?: unknown;
            /** Format: uuid */
            id: string;
            request_kind: string;
            request_payload: unknown;
            state: string;
        };
        RequestView: {
            /** Format: uuid */
            conversation_id: string;
            created_at: string;
            decision_payload?: unknown;
            /** Format: uuid */
            execution_id: string;
            /** Format: int64 */
            generation: number;
            /** Format: uuid */
            id: string;
            kind: string;
            native_request_id: string;
            request_payload: unknown;
            state: string;
            updated_at: string;
            /** Format: uuid */
            winner_decision_id?: string | null;
        };
        /** @description Последнее разрешение псевдонима на машине. */
        ResolutionView: {
            /** @description Когда псевдоним сменил разрешение. */
            changed_at?: string | null;
            /** @description Конкретная модель, например `claude-fable-5-2`. */
            model: string;
            observed_at: string;
            /** @description Прежнее разрешение, если псевдоним его сменил. */
            previous?: string | null;
        };
        ResolveOwnerLostOutcome: {
            requested: {
                /** Format: uuid */
                command_id: string;
            };
        } | {
            already_requested: {
                /** Format: uuid */
                command_id: string;
            };
        } | "not_owner_lost";
        ResumeBody: {
            continue_queue?: boolean;
        };
        /** @enum {string} */
        ResyncReason: "cursor_too_old" | "cursor_ahead";
        ResyncRequired: {
            oldest_available_seq: string;
            reason: components["schemas"]["ResyncReason"];
        };
        /** @enum {string} */
        Retry: "safe_same_key" | "explicit_new_operation" | "after_condition" | "never";
        RevokedView: {
            /** @description Отозванные сеансы, включая каскад. */
            revoked: string[];
        };
        RoutingPolicyBody: {
            policy: components["schemas"]["Policy"] | null;
            /** Format: int64 */
            revision: number;
        };
        /** @description Политика владельца: `null` — встроенная; ответ всегда несёт и встроенную. */
        RoutingPolicyView: {
            builtin: components["schemas"]["Policy"];
            policy?: components["schemas"]["Policy"] | null;
            /** Format: int64 */
            revision: number;
        };
        /** @description Предпросмотр выбора для типа задачи; `workdir` переопределяет каталог родителя. */
        RoutingPreviewBody: {
            level?: string | null;
            task_type: string;
            workdir?: string | null;
        };
        /** @description Решение маршрута и запас машины; та же форма, что у `route_preview`. */
        RoutingPreviewView: {
            considered: components["schemas"]["Considered"][];
            decision: components["schemas"]["Decision"];
            guard: components["schemas"]["GuardVerdict"];
            machine: components["schemas"]["MachineHeadroom"];
        };
        RunInfo: {
            /** Format: int32 */
            compactions?: number | null;
            context?: components["schemas"]["ContextInfo"] | null;
            /** Format: int64 */
            duration_ms?: number | null;
            /** Format: date-time */
            finished_at?: string | null;
            /** Format: uuid */
            input_id: string;
            limit?: components["schemas"]["RunLimit"] | null;
            parts: components["schemas"]["PartView"][];
            /** Format: uuid */
            run_id: string;
            /** @description Скорость ответа по моделям основного агента; пусто, если её нельзя выделить честно. */
            speed: components["schemas"]["ModelSpeed"][];
            /** Format: date-time */
            started_at: string;
            usage: components["schemas"]["ModelUsageView"][];
        };
        RunLimit: {
            exact: boolean;
            provider: string;
            /** Format: int32 */
            weekly_delta_permille: number;
        };
        RunSlot: "free" | {
            dispatching: {
                /** Format: int32 */
                attempt: number;
                /** Format: uuid */
                input_id: string;
            };
        } | {
            running: {
                /** Format: uuid */
                input_id: string;
            };
        };
        ScheduleBody: {
            attachment_ids?: string[];
            content: components["schemas"]["ContentBlock"][];
            /** @description Устройство, отправившее текст из композера: свой `draft_changed` оно игнорирует. */
            device_id?: string | null;
            /** Format: date-time */
            run_at: string;
        };
        ScheduledMessage: {
            attachment_ids: string[];
            /** Format: int32 */
            attempts: number;
            content: components["schemas"]["ContentBlock"][];
            /** Format: uuid */
            conversation_id: string;
            /** Format: uuid */
            id: string;
            /** Format: uuid */
            input_id?: string | null;
            /** Format: date-time */
            run_at: string;
            state: components["schemas"]["JobState"];
        };
        /** @description Поиск по ответам агента и сообщениям ленты; новые первыми. */
        SearchBody: {
            /** Format: uuid */
            conversation_id?: string | null;
            /** @description `next_cursor` предыдущей страницы. */
            cursor?: string | null;
            harness?: string | null;
            kind?: components["schemas"]["SearchKind"] | null;
            /**
             * Format: int64
             * @description 1–50, по умолчанию 20.
             */
            limit?: number | null;
            /** @description Слова запроса, до 200 символов: кавычки ищут фразу, `-слово` исключает, `or` — любое. */
            q: string;
            /** Format: date-time */
            since?: string | null;
            /** Format: date-time */
            until?: string | null;
        };
        /**
         * @description Что искать: ответы агента, сообщения ленты или (без поля) и то и другое.
         * @enum {string}
         */
        SearchKind: "answers" | "messages";
        SearchPage: {
            hits: components["schemas"]["Hit"][];
            next_cursor: string | null;
            /**
             * Format: int64
             * @description Число совпадений, не больше 200.
             */
            total: number;
            /** @description Совпадений больше, чем `total`. */
            total_capped: boolean;
        };
        SendNowOutcome: {
            requested: {
                /** Format: uuid */
                steer_command_id: string;
            };
        } | {
            already_requested: {
                /** Format: uuid */
                steer_command_id: string;
            };
        };
        /**
         * @description Место по IP из локальной базы DB-IP: русские названия, где они есть, иначе английские.
         *     `local`: частная, петлевая, link-local или CGNAT сеть («локальная сеть»), остальные поля пусты.
         */
        SessionLocationView: {
            city?: string | null;
            country?: string | null;
            country_code?: string | null;
            local?: boolean;
            region?: string | null;
        };
        SessionUsageView: {
            /** @description Все делегаты в поддереве: живые потомки и свёрнутые после удаления. */
            delegates: components["schemas"]["UsageTotalsView"];
            /** @description Собственный расход сессии: субагенты внутри CLI уже входят в её ходы. */
            session: components["schemas"]["UsageTotalsView"];
        };
        SessionView: {
            activity_state: string;
            /** Format: uuid */
            conversation_id: string;
            title: string;
        };
        SessionsView: {
            /**
             * Format: int64
             * @description Сколько сессия лежит в корзине до окончательной уборки; `0` — уборка выключена.
             */
            trash_retention_secs: number;
        };
        SetPasswordBody: {
            /** @description Обязателен, если пароль уже задан. */
            current_password?: string | null;
            /** @description 10..1024 байт. */
            new_password: string;
        };
        SettingsPatch: {
            effort?: string | null;
            instruction_profile?: string | null;
            model?: string | null;
            permission_mode?: string | null;
        };
        SettingsView: {
            effective: components["schemas"]["EffectiveSettings"];
            pending: unknown;
            /** Format: int64 */
            rev: number;
        };
        /**
         * @description Способ входа, создавший сеанс.
         * @enum {string}
         */
        SignInMethod: "password" | "passkey" | "qr" | "recovery";
        SignInView: {
            session: components["schemas"]["OwnerSessionView"];
        };
        SnapshotQueueView: {
            hold_reasons: components["schemas"]["HoldView"][];
            pause_reasons: components["schemas"]["PauseReason"][];
            queued: components["schemas"]["InputView"][];
            run_slot: components["schemas"]["RunSlot"];
        };
        Snapshot_ConversationSnapshot: {
            cursor: string;
            data: {
                /** @description Взведённые и недавние завершившиеся будильники разговора. */
                alarms: components["schemas"]["AlarmView"][];
                background: components["schemas"]["BackgroundTaskView"][];
                context?: components["schemas"]["ContextSnapshot"] | null;
                detail: components["schemas"]["ConversationDetail"];
                draft?: components["schemas"]["DraftView"] | null;
                history: components["schemas"]["HistoryPage"];
                nudge?: components["schemas"]["NudgeRuleView"] | null;
                nudge_events: components["schemas"]["NudgeEventView"][];
                /** @description Незавершённые запросы по времени создания: открытые, с сохранённым и с передаваемым агенту решением. */
                open_requests: components["schemas"]["RequestView"][];
                prompt_suggestion?: components["schemas"]["PromptSuggestionView"] | null;
                queue: components["schemas"]["SnapshotQueueView"];
                scheduled: components["schemas"]["ScheduledMessage"][];
                /** @description Очередь запусков этого разговора: ожидающие и недавно завершённые. */
                spawn_queue: components["schemas"]["QueueItemView"][];
                /** @description Списки задач агента по дорожкам, основной агент первым. */
                todos: components["schemas"]["TodoListView"][];
            };
            scope: string;
        };
        Snapshot_ListSnapshot: {
            cursor: string;
            data: {
                counts: components["schemas"]["Counts"];
                groups: components["schemas"]["Group"][];
                items: components["schemas"]["ListItem"][];
                limits: components["schemas"]["ProviderView"][];
                /**
                 * @description Задачи досок плана корневых оркестраторов вне архива и корзины; готовые и
                 *     отменённые — за 7 дней, не больше 50 оркестраторов. Снимок заменяет набор целиком.
                 */
                plan_tasks: components["schemas"]["PlanTask"][];
                presets: components["schemas"]["PresetSummary"][];
                truncated: boolean;
            };
            scope: string;
        };
        Snapshot_MachinesSnapshot: {
            cursor: string;
            data: {
                items: components["schemas"]["MachineView"][];
            };
            scope: string;
        };
        /** @description Кусок фрагмента; `hit` — совпавшее слово, клиент его подсвечивает. HTML сервер не шлёт. */
        SnippetSegment: {
            hit: boolean;
            text: string;
        };
        SpawnQueueActionView: {
            harness?: string | null;
            model?: string | null;
            /** Format: uuid */
            queue_id: string;
            /** Format: uuid */
            session_id?: string | null;
            status: string;
            title?: string | null;
        };
        SpawnQueueView: {
            items: components["schemas"]["QueueItemView"][];
        };
        /** @description Структурный отказ «Запустить сейчас»: код, сообщение и состояние элемента. */
        SpawnRefusal: {
            code: components["schemas"]["SpawnRefusalCode"];
            message: string;
            /**
             * Format: date-time
             * @description Когда элемент снова попробуют запустить (`queued`).
             */
            retry_at?: string;
            /** @description Состояние элемента после отказа: `queued`, `failed`, `cancelled` или `expired`. */
            state: string;
        };
        /**
         * @description Код отказа «Запустить сейчас». Замкнутое перечисление: `details.spawn_refusal.code`
         *     не превращается в свободный текст. Значения совпадают с кодами инструментов запуска
         *     и состояниями элемента очереди.
         * @enum {string}
         */
        SpawnRefusalCode: "not_in_whitelist" | "permission_mode_too_wide" | "machine_unavailable" | "capability_missing" | "no_executor_available" | "invalid_arguments" | "harness_unknown" | "model_unknown" | "effort_not_allowed" | "workdir_not_allowed" | "profile_unknown" | "conversation_closed" | "queue_full" | "rate_limited" | "machine_busy" | "parent_stopped" | "parent_archived" | "cancelled" | "expired" | "job_failed" | "internal_error" | "unknown";
        SpeedStats: {
            /**
             * Format: int64
             * @description Медиана по ходам.
             */
            first_token_ms?: number | null;
            /**
             * Format: int64
             * @description Ходы, по которым посчитана скорость генерации.
             */
            runs: number;
            /**
             * Format: double
             * @description Медиана по ходам.
             */
            tokens_per_second?: number | null;
        };
        /** @enum {string} */
        SpendOutcome: "continued" | "already_continued" | "not_held";
        StartedExecution: {
            /** Format: uuid */
            command_id: string;
            /** Format: uuid */
            execution_id: string;
            /** Format: int64 */
            generation: number;
        };
        StopBody: {
            force?: boolean;
        };
        StopExecutionOutcome: {
            requested: {
                /** Format: uuid */
                command_id: string;
                force: boolean;
            };
        } | {
            already_requested: {
                /** Format: uuid */
                command_id: string;
            };
        } | "nothing_to_stop" | "owner_lost";
        StopOutcome: {
            requested: {
                /** Format: uuid */
                interrupt_command_id?: string | null;
                /** Format: uuid */
                withdraw_command_id?: string | null;
            };
        };
        SubagentRef: {
            model: string;
            name: string;
        };
        SubmitBody: {
            attachment_ids?: string[];
            content: components["schemas"]["ContentBlock"][];
            /** @description Устройство, отправившее текст из композера: свой `draft_changed` оно игнорирует. */
            device_id?: string | null;
            /** Format: uuid */
            retry_of?: string | null;
        };
        SubmittedInput: {
            /** Format: uuid */
            command_id: string;
            /** Format: uuid */
            input_id: string;
            /** Format: int64 */
            input_seq: number;
            state: components["schemas"]["InputState"];
        };
        /** @enum {string} */
        TargetKind: "input" | "block";
        /** @description Тип задачи: русское название для PWA и упорядоченные кандидаты. */
        TaskType: {
            candidates: components["schemas"]["Candidate"][];
            /**
             * Format: int64
             * @description Ожидаемая длительность задачи в минутах: по ней прогноз решает, хватит ли лимита.
             */
            expected_minutes?: number;
            isolated?: boolean | null;
            preset?: string | null;
            title: string;
        };
        TimelineItem: (components["schemas"]["InputView"] & {
            /** @enum {string} */
            kind: "input";
        }) | (components["schemas"]["BlockView"] & {
            /** @enum {string} */
            kind: "block";
        }) | (components["schemas"]["RequestItemView"] & {
            /** @enum {string} */
            kind: "request";
        }) | (components["schemas"]["CompactionView"] & {
            /** @enum {string} */
            kind: "compaction";
        });
        TodoItemView: {
            status: components["schemas"]["TodoStatus"];
            text: string;
        };
        TodoListView: {
            items: components["schemas"]["TodoItemView"][];
            /**
             * Format: uuid
             * @description Блок вызова субагента; у основного агента `null`.
             */
            lane_block_id?: string | null;
            /**
             * Format: int64
             * @description Сколько пунктов не поместилось в запись воркера.
             */
            omitted: number;
            /**
             * Format: int64
             * @description Растёт с каждым принятым обновлением дорожки.
             */
            revision: number;
        };
        /** @enum {string} */
        TodoStatus: "pending" | "in_progress" | "completed" | "cancelled";
        /** @enum {string} */
        UnknownLimits: "allow" | "skip";
        UploadOffer: {
            /** Format: int64 */
            offset: number;
            /** Format: int64 */
            upload_chunk_bytes: number;
        };
        UsageDay: {
            agent: string;
            /** Format: int64 */
            cache_read_tokens: number;
            /** Format: int64 */
            cache_write_tokens: number;
            /** Format: uuid */
            conversation_id: string;
            /** Format: date */
            day: string;
            /** Format: int64 */
            input_tokens: number;
            model: string;
            /** Format: int64 */
            output_tokens: number;
            /** Format: int64 */
            overlapped_runs: number;
            provider: string;
            /** Format: int64 */
            runs: number;
            /** Format: int64 */
            weekly_delta_permille: number;
        };
        UsageTotalsView: {
            /** Format: int64 */
            cache_read_tokens: number;
            /** Format: int64 */
            cache_write_tokens: number;
            /** Format: int64 */
            input_tokens: number;
            models: components["schemas"]["ModelUsageView"][];
            /** Format: int64 */
            output_tokens: number;
            /** Format: int64 */
            turns: number;
        };
        WatchView: {
            hotspots: components["schemas"]["HotspotView"][];
            leaked: components["schemas"]["LeakedView"][];
            observed_at: string;
            /** Format: int32 */
            unleased_count: number;
        };
        WhitelistBody: {
            /** @description `null` возвращает встроенный список; поле обязательно, чтобы пустое тело его не сбрасывало. */
            entries: components["schemas"]["WhitelistEntry"][] | null;
        };
        WhitelistEntry: {
            harness: string;
            model: string;
        };
        WhitelistView: {
            default_entries: components["schemas"]["WhitelistEntry"][];
            entries: components["schemas"]["WhitelistEntry"][];
            is_default: boolean;
        };
        WindowView: {
            forecast?: components["schemas"]["ForecastView"] | null;
            observed_at: string;
            resets_at?: string | null;
            /** Format: double */
            used_pct: number;
            /** Format: int32 */
            used_permille: number;
        };
        WireEvent: {
            committed_at: string;
            committed_at_from: string;
            /** Format: uuid */
            conversation_id?: string | null;
            /** Format: uuid */
            correlation_id: string;
            /** Format: uuid */
            execution_id?: string | null;
            /** Format: uuid */
            id: string;
            kind: string;
            payload: unknown;
            scope: string;
            scope_seq: string;
            scope_seq_from: string;
            /** Format: int32 */
            v: number;
        };
        Workdirs: {
            workdirs: string[];
        };
        /** @description Рабочая копия делегированной сессии: остальное из `conversations.worktree` хабу для себя. */
        WorktreeView: {
            branch: string;
            path: string;
            state: string;
        };
        WsView: {
            /** Format: int64 */
            delta_window_ms: number;
            path: string;
            protocol: string;
        };
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    post__api_v1_attachments: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["NewAttachment"];
            };
        };
        responses: {
            /** @description Вложение создано, загрузка с нулевого смещения */
            201: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedAttachment"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_attachments__id_: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Вложение */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AttachmentRecord"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_attachments__id_: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Загрузка отменена или неиспользованное вложение удалено */
            204: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_attachments__id__complete: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        /** @description Необязательное тело */
        requestBody: {
            content: {
                "application/json": components["schemas"]["CompleteBody"];
            };
        };
        responses: {
            /** @description Содержимое проверено, надёжно опубликовано, вложение готово */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AttachmentRecord"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_attachments__id__content: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Содержимое готового вложения: проверенная картинка показывается на месте, остальное скачивается; имя файла в Content-Disposition */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/octet-stream": components["schemas"]["BinaryContent"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    head__api_v1_attachments__id__content: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Принятое смещение загрузки */
            200: {
                headers: {
                    /** @description Объявленный размер */
                    "Upload-Length"?: number;
                    /** @description Принятые байты */
                    "Upload-Offset"?: number;
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_attachments__id__content: {
        parameters: {
            query?: never;
            header: {
                /** @description Смещение, с которого продолжается загрузка */
                "Upload-Offset": number;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        /** @description Не больше upload_chunk_bytes */
        requestBody: {
            content: {
                "application/octet-stream": components["schemas"]["BinaryContent"];
            };
        };
        responses: {
            /** @description Часть принята */
            204: {
                headers: {
                    /** @description Принятые байты */
                    "Upload-Offset"?: number;
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_logout: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сеанс завершён */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RevokedView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_auth_methods: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Способы входа */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MethodsView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_passkey_finish: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PasskeySignIn"];
            };
        };
        responses: {
            /** @description Вход выполнен; Set-Cookie сеанса и устройства */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SignInView"];
                };
            };
            /** @description invalid_credentials */
            401: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description too_many_attempts */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_passkey_start: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PasskeyStartBody"];
            };
        };
        responses: {
            /** @description Опции для navigator.credentials.get() */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeyChallengeView"];
                };
            };
            /** @description no_passkeys или passkeys_unavailable */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description too_many_attempts */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_auth_passkeys: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Ключи доступа владельца */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeysView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_passkeys_register_finish: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PasskeyRegisterFinish"];
            };
        };
        responses: {
            /** @description Ключ добавлен */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeyView"];
                };
            };
            /** @description invalid_request: имя */
            400: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description passkey_rejected или reauth_required */
            403: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_passkeys_register_start: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PasskeyStartBody"];
            };
        };
        responses: {
            /** @description Опции для navigator.credentials.create() */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeyChallengeView"];
                };
            };
            /** @description reauth_required */
            403: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description passkeys_unavailable */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_auth_passkeys__id_: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Ключ удалён; оставшиеся ключи */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeysView"];
                };
            };
            /** @description reauth_required */
            403: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ключа нет */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_auth_passkeys__id_: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RenamePasskey"];
            };
        };
        responses: {
            /** @description Имя сохранено */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeyView"];
                };
            };
            /** @description invalid_request */
            400: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ключа нет */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_auth_password: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetPasswordBody"];
            };
        };
        responses: {
            /** @description Пароль сохранён; отозванные сеансы */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RevokedView"];
                };
            };
            /** @description wrong_password или reauth_required */
            403: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description too_many_attempts */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_password: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PasswordSignIn"];
            };
        };
        responses: {
            /** @description Вход выполнен; Set-Cookie сеанса и устройства */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SignInView"];
                };
            };
            /** @description invalid_credentials */
            401: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description too_many_attempts, details.retry_after_seconds */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_qr_approve: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["QrSecretBody"];
            };
        };
        responses: {
            /** @description Вход разрешён */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["QrDecisionView"];
                };
            };
            /** @description qr_approvals_exhausted: этот сеанс уже подтвердил 5 живых входов */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description qr_expired */
            410: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_qr_deny: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["QrSecretBody"];
            };
        };
        responses: {
            /** @description Вход отклонён */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["QrDecisionView"];
                };
            };
            /** @description qr_expired */
            410: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_qr_inspect: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["QrSecretBody"];
            };
        };
        responses: {
            /** @description Запрос на вход */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["QrRequestView"];
                };
            };
            /** @description qr_expired: кода нет, он истёк или уже решён */
            410: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_qr_poll: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["QrPollBody"];
            };
        };
        responses: {
            /** @description Состояние кода; при approved — Set-Cookie сеанса */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["QrPollView"];
                };
            };
            /** @description too_many_attempts: в этом процессе хаба уже ждут 8 опросов */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_qr_start: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["QrStartBody"];
            };
        };
        responses: {
            /** @description Код создан */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["QrStartView"];
                };
            };
            /** @description too_many_attempts, details.retry_after_seconds */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_reauth_passkey_finish: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PasskeyReauthFinish"];
            };
        };
        responses: {
            /** @description Сеанс свеж */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FreshView"];
                };
            };
            /** @description passkey_rejected */
            403: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description too_many_attempts */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_reauth_passkey_start: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PasskeyStartBody"];
            };
        };
        responses: {
            /** @description Опции для navigator.credentials.get() */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeyChallengeView"];
                };
            };
            /** @description no_passkeys или passkeys_unavailable */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_reauth_password: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ReauthPassword"];
            };
        };
        responses: {
            /** @description Сеанс свеж */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FreshView"];
                };
            };
            /** @description wrong_password */
            403: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description too_many_attempts */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_recovery: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RecoverySignIn"];
            };
        };
        responses: {
            /** @description Вход выполнен; Set-Cookie сеанса и устройства */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SignInView"];
                };
            };
            /** @description invalid_credentials: токен неизвестен, использован или истёк */
            401: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description too_many_attempts */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_auth_sessions: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Живые сеансы */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["OwnerSessionsView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_auth_sessions_revoke_others: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Остальные сеансы завершены */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RevokedView"];
                };
            };
            /** @description reauth_required */
            403: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_auth_sessions__id_: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сеанс завершён */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RevokedView"];
                };
            };
            /** @description reauth_required */
            403: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Живого сеанса нет */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_auth_sessions__id_: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RenameSession"];
            };
        };
        responses: {
            /** @description Имя сохранено */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["OwnerSessionView"];
                };
            };
            /** @description Живого сеанса нет */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_auth_settings: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AuthSettingsBody"];
            };
        };
        responses: {
            /** @description Настройки сохранены */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AuthSettingsView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_bookmarks: {
        parameters: {
            query?: {
                /** @description 1–100, по умолчанию 50. */
                limit?: number;
                /** @description `next_cursor` предыдущей страницы. */
                cursor?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Избранное владельца, новые закладки первыми */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BookmarkPage"];
                };
            };
            /** @description Неверный курсор или размер страницы */
            400: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_config: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Настройки для клиента */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConfigView"];
                };
            };
            /** @description Нет сеанса */
            401: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations: {
        parameters: {
            query?: {
                filter?: string;
                q?: string;
                cursor?: string;
                limit?: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Список сессий */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ListPage"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateBody"];
            };
        };
        responses: {
            /** @description Сессия создана */
            201: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Created"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id_: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сессия */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationDetail"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_conversations__id_: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сессия удалена; повтор с тем же ключом даёт тот же ответ */
            204: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description not_found или conversation_deleted */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description conversation_live: исполнение не остановлено; worktree_not_removed: рабочая копия ещё на машине */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_conversations__id_: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["Attributes"];
            };
        };
        responses: {
            /** @description Атрибуты изменены */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationDetail"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__accept: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AcceptBody"];
            };
        };
        responses: {
            /** @description Приём результата делегированной сессии начат */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AcceptView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__alarms: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Будильники разговора */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AlarmList"];
                };
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_conversations__id__alarms__alarm_id_: {
        parameters: {
            query?: never;
            header: {
                /** @description Ключ операции */
                "Idempotency-Key": string;
            };
            path: {
                /** @description Разговор */
                id: string;
                /** @description Будильник */
                alarm_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Будильник отменён или уже завершён */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AlarmView"];
                };
            };
            /** @description Будильник не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__blocks__block_id_: {
        parameters: {
            query?: {
                offset?: number;
                limit?: number;
            };
            header?: never;
            path: {
                id: string;
                block_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Текст блока; смещения в байтах UTF-8 */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BlockPage"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__bookmarks: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Все закладки разговора в порядке ленты, до 500 */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BookmarkView"][];
                };
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_conversations__id__bookmarks__kind___target_id_: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
                /** @description input или block */
                kind: string;
                /** @description Ввод или верхний блок ленты */
                target_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Такая закладка уже есть */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BookmarkView"];
                };
            };
            /** @description Закладка создана */
            201: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BookmarkView"];
                };
            };
            /** @description Разговор или цель не найдены */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Неизвестный вид цели */
            422: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_conversations__id__bookmarks__kind___target_id_: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
                /** @description input или block */
                kind: string;
                /** @description Ввод или верхний блок ленты */
                target_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Закладка удалена; повтор безвреден */
            204: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Неизвестный вид цели */
            422: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__compact: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сжатие контекста запрошено */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CompactOutcome"];
                };
            };
            /** @description Конфликт состояния, причина в details.reason */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_conversations__id__crunch: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CrunchSetting"];
            };
        };
        responses: {
            /** @description Кранч изменён */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationDetail"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__delegation: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Вызовы субагентов */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DelegationNode"][];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__draft: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Черновик поля ввода; пустой текст значит, что черновика нет */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DraftView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_conversations__id__draft: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PutDraft"];
            };
        };
        responses: {
            /** @description Черновик сохранён; пустой текст удаляет его */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DraftView"];
                };
            };
            /** @description Текст длиннее 64 КиБ или вложение не готово/чужое */
            400: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__executions: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Исполнение создано */
            201: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["StartedExecution"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_conversations__id__groups: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["GroupsBody"];
            };
        };
        responses: {
            /** @description Группы изменены */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationDetail"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_conversations__id__groups__group_id__pin: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
                group_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сессия закреплена в группе */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationDetail"];
                };
            };
            /** @description not_found или group_not_found */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_conversations__id__groups__group_id__pin: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
                group_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сессия откреплена в группе */
            204: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description not_found или group_not_found */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__history: {
        parameters: {
            query?: {
                before?: string;
                after?: string;
                /** @description Место в ленте: окно вокруг него вместо страницы от края. */
                around?: string;
                limit?: number;
            };
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Окно истории в порядке scope_seq */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["HistoryPage"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__inputs: {
        parameters: {
            query?: {
                before?: string;
                limit?: number;
            };
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сообщения владельца в порядке ленты */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["OwnerInputsPage"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__inputs: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SubmitBody"];
            };
        };
        responses: {
            /** @description Сообщение в очереди */
            201: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SubmittedInput"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_conversations__id__inputs__input_id_: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
                input_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["EditInputBody"];
            };
        };
        responses: {
            /** @description Ожидающий ввод исправлен */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EditInputOutcome"];
                };
            };
            /** @description Конфликт состояния, причина в details.reason */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__inputs__input_id__send_now: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
                input_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Ожидающий ввод влит в идущий ход */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SendNowOutcome"];
                };
            };
            /** @description Конфликт состояния, причина в details.reason */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__interrupt: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Остановка запрошена */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["StopOutcome"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__lanes__block_id_: {
        parameters: {
            query?: {
                before?: string;
                limit?: number;
            };
            header?: never;
            path: {
                id: string;
                block_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Страница ленты субагента: корень и потомки от нового к старому */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LanePage"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__nudge: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Правило автопинка и последние события */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NudgeState"];
                };
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_conversations__id__nudge: {
        parameters: {
            query?: never;
            header: {
                /** @description Ключ операции */
                "Idempotency-Key": string;
            };
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["OwnerUpdate"];
            };
        };
        responses: {
            /** @description Правило сохранено */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NudgeRuleView"];
                };
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Некорректные значения */
            422: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_conversations__id__nudge: {
        parameters: {
            query?: never;
            header: {
                /** @description Ключ операции */
                "Idempotency-Key": string;
            };
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Правило снято */
            204: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__nudge_events: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Последние события автопинка */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["NudgeEvents"];
                };
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__queue_continue: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Очередь продолжена */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ContinueOutcome"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__read: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ReadBody"];
            };
        };
        responses: {
            /** @description Прочитанность сохранена */
            204: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__requests__request_id__decision: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
                /** @description 1 */
                "X-Canterlot-Client": string;
            };
            path: {
                id: string;
                request_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["DecisionBody"];
            };
        };
        responses: {
            /** @description Решение сохранено; повтор возвращает первый ответ */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DecisionOutcomeView"];
                };
            };
            /** @description Некорректный запрос */
            400: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Запрос или сессия не найдены */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description decision_conflict, request_not_open или idempotency_conflict */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description decision_invalid: ответ не подходит к контракту */
            422: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__resolve_owner_lost: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Решение по потерянному исполнению передано машине */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ResolveOwnerLostOutcome"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__restore: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сессия возвращена из корзины */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationDetail"];
                };
            };
            /** @description not_found или conversation_deleted */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__resume: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ResumeBody"];
            };
        };
        responses: {
            /** @description Исполнение возобновлено */
            201: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["StartedExecution"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__runs__run_id_: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                run_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сведения о ходе */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RunInfo"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__scheduled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Отложенные сообщения */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ScheduledMessage"][];
                };
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__scheduled: {
        parameters: {
            query?: never;
            header: {
                /** @description Ключ операции */
                "Idempotency-Key": string;
            };
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ScheduleBody"];
            };
        };
        responses: {
            /** @description Сообщение запланировано */
            201: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ScheduledMessage"];
                };
            };
            /** @description Конфликт или слишком много сообщений */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Время вне допустимого диапазона */
            422: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_conversations__id__settings: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SettingsPatch"];
            };
        };
        responses: {
            /** @description Настройки изменены */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SettingsView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__snapshot: {
        parameters: {
            query?: {
                history_limit?: number;
            };
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Снимок разговора и граница журнала */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Snapshot_ConversationSnapshot"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__spawn_queue: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Разговор */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Очередь запусков разговора */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SpawnQueueView"];
                };
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_conversations__id__spawn_queue__queue_id_: {
        parameters: {
            query?: never;
            header: {
                /** @description Ключ операции */
                "Idempotency-Key": string;
            };
            path: {
                /** @description Разговор */
                id: string;
                /** @description Элемент очереди */
                queue_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Элемент отменён или уже завершён */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SpawnQueueActionView"];
                };
            };
            /** @description Элемент не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__spawn_queue__queue_id__start_now: {
        parameters: {
            query?: never;
            header: {
                /** @description Ключ операции */
                "Idempotency-Key": string;
            };
            path: {
                /** @description Разговор */
                id: string;
                /** @description Элемент очереди */
                queue_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сессия запущена */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SpawnQueueActionView"];
                };
            };
            /** @description Элемент не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Элемент уже завершён или запуск отклонён: `details.reason` = `spawn_refused`, разбор в `details.spawn_refusal` */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__spend_reserve: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Решение о продолжении за счёт резерва */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SpendOutcome"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_conversations__id__stop: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["StopBody"];
            };
        };
        responses: {
            /** @description Остановка исполнения запрошена */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["StopExecutionOutcome"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_conversations__id__trash: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сессия удалена окончательно */
            204: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description not_found или conversation_deleted */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description invalid_state: сессия не в корзине */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_conversations__id__usage: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Токены за сессию и её делегатов */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SessionUsageView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_events: {
        parameters: {
            query: {
                scope: string;
                after: string;
                limit?: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Диапазон событий либо требование нового снимка */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RangeResponse"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_groups: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Группы */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Group"][];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_groups: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["GroupBody"];
            };
        };
        responses: {
            /** @description Группа создана */
            201: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Group"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_groups__id_: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Группа удалена */
            204: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_groups__id_: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["GroupPatch"];
            };
        };
        responses: {
            /** @description Группа изменена */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Group"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_inputs__id__cancel: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Отмена запрошена */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CancelOutcome"];
                };
            };
            /** @description Конфликт состояния, причина в details.reason */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_limit_settings: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Пороги лимитов владельца */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LimitSettingsView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_limit_settings: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["LimitSettingsBody"];
            };
        };
        responses: {
            /** @description Пороги сохранены */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LimitSettingsView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_limits: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Текущие лимиты провайдеров, чьи харнессы есть в каталоге машин владельца */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProviderView"][];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_limits_history: {
        parameters: {
            query?: {
                provider?: string;
                from?: string;
                to?: string;
                tz?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Расход и история лимитов */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["History"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_limits_refresh: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Просьба об опросе отправлена воркерам; новые показания придут событием limits_changed */
            202: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RefreshView"];
                };
            };
            /** @description Нет доступного воркера с лимитами */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Опрос просили слишком недавно */
            429: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_machines: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Машины владельца */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MachineView"][];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_machines__id_: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Машина */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MachineView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_machines__id__claude_histories: {
        parameters: {
            query: {
                workdir: string;
            };
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Беседы Claude Code рабочего каталога, свежие первыми; пусто, если машина не ответила */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ClaudeHistoryList"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_machines__id__owner_instructions: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Метаданные инструкций владельца и проекты памяти */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["OwnerOverviewView"];
                };
            };
            /** @description Воркер без возможности или машина недоступна */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_machines__id__owner_instructions_file: {
        parameters: {
            query: {
                kind: string;
                project?: string;
                name?: string;
            };
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Содержимое одного файла инструкций владельца */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["OwnerFileContent"];
                };
            };
            /** @description Воркер без возможности или машина недоступна */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_machines__id__workdirs_recent: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Последние каталоги для экрана новой сессии */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Workdirs"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_machines__id__workdirs_suggest: {
        parameters: {
            query: {
                path: string;
            };
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Существующие каталоги машины по введённому пути; пустой список, если машина не ответила */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Workdirs"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_me: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Владелец */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MeView"];
                };
            };
            /** @description Нет сеанса */
            401: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_media__id_: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Картинка, на которую смотрел агент: тип из проверенного списка, показ на месте, ETag равен SHA-256 */
            200: {
                headers: {
                    /** @description SHA-256 содержимого в кавычках */
                    ETag?: string;
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "image/png": components["schemas"]["BinaryContent"];
                    "image/jpeg": components["schemas"]["BinaryContent"];
                    "image/webp": components["schemas"]["BinaryContent"];
                    "image/gif": components["schemas"]["BinaryContent"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_openapi_json: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Документ OpenAPI */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": unknown;
                };
            };
            /** @description Нет сеанса */
            401: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_plan: {
        parameters: {
            query: {
                /** @description Разговор оркестратора. */
                orchestrator: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Доска плана оркестратора с коммитами и отчётом исполнителей */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PlanList"];
                };
            };
            /** @description Разговор не найден */
            404: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_presets: {
        parameters: {
            query?: {
                purpose?: components["schemas"]["PresetPurpose"];
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Пресеты владельца без текста */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PresetSummary"][];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_presets: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PresetBody"];
            };
        };
        responses: {
            /** @description Пресет создан */
            201: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Preset"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_presets__id_: {
        parameters: {
            query?: {
                purpose?: components["schemas"]["PresetPurpose"];
            };
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Пресет; текст только у пользовательского */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Preset"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_presets__id_: {
        parameters: {
            query?: {
                purpose?: components["schemas"]["PresetPurpose"];
            };
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Пресет удалён, сессии держат копию */
            204: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_presets__id_: {
        parameters: {
            query?: {
                purpose?: components["schemas"]["PresetPurpose"];
            };
            header: {
                "Idempotency-Key": string;
            };
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PresetPatchBody"];
            };
        };
        responses: {
            /** @description Пресет изменён */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Preset"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_prompt_suggestion_settings: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Подсказки следующего сообщения владельца */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PromptSuggestionSettings"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_prompt_suggestion_settings: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PromptSuggestionSettings"];
            };
        };
        responses: {
            /** @description Настройка сохранена; выключение стирает показанные подсказки */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PromptSuggestionSettings"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_routing_policy: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Политика маршрутизации владельца */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RoutingPolicyView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_routing_policy: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RoutingPolicyBody"];
            };
        };
        responses: {
            /** @description Политика сохранена */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RoutingPolicyView"];
                };
            };
            /** @description Устаревшая ревизия */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_routing_policy_preview: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RoutingPreviewBody"];
            };
        };
        responses: {
            /** @description Выбор маршрута и запас машины */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RoutingPreviewView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    delete__api_v1_scheduled__id_: {
        parameters: {
            query?: never;
            header: {
                /** @description Ключ операции */
                "Idempotency-Key": string;
            };
            path: {
                /** @description Отложенное сообщение */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Сообщение отменено */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ScheduledMessage"];
                };
            };
            /** @description Сообщение уже обработано */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    patch__api_v1_scheduled__id_: {
        parameters: {
            query?: never;
            header: {
                /** @description Ключ операции */
                "Idempotency-Key": string;
            };
            path: {
                /** @description Отложенное сообщение */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PatchScheduled"];
            };
        };
        responses: {
            /** @description Сообщение изменено */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ScheduledMessage"];
                };
            };
            /** @description Сообщение уже обработано */
            409: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    post__api_v1_search: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SearchBody"];
            };
        };
        responses: {
            /** @description Совпадения, новые первыми */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SearchPage"];
                };
            };
            /** @description Пустой или длинный запрос, неверный курсор, размер страницы или период */
            400: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Поиск не уложился в срок */
            503: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_snapshot_conversations: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Снимок списка и граница журнала */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Snapshot_ListSnapshot"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_snapshot_machines: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Снимок машин и граница журнала */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Snapshot_MachinesSnapshot"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_spawn_whitelist: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Белый список делегирования */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["WhitelistView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    put__api_v1_spawn_whitelist: {
        parameters: {
            query?: never;
            header: {
                "Idempotency-Key": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["WhitelistBody"];
            };
        };
        responses: {
            /** @description Список сохранён */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["WhitelistView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__api_v1_ws: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description WSS canterlot.v1; кадры ClientOp / ServerOp в events.schema.json */
            101: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__healthz: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Процесс жив */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProbeView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
    get__readyz: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Хаб готов */
            200: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProbeView"];
                };
            };
            /** @description База или фоновая служба недоступна, либо установка не совпадает */
            503: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProbeView"];
                };
            };
            /** @description Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry */
            default: {
                headers: {
                    /** @description Часы хаба: целые миллисекунды Unix */
                    "X-Server-Time-Ms"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorEnvelope"];
                };
            };
        };
    };
}
