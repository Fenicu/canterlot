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
        delete?: never;
        options?: never;
        head?: never;
        patch: operations["patch__api_v1_conversations__id_"];
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
        get?: never;
        put?: never;
        post: operations["post__api_v1_conversations__id__inputs"];
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
            archived: boolean;
            color?: string | null;
            created_at: string;
            groups: string[];
            harness: components["schemas"]["HarnessRef"];
            /** Format: uuid */
            id: string;
            machine: components["schemas"]["MachineRef"];
            pinned: boolean;
            title: string;
            title_source: string;
            workdir: string;
        };
        Attributes: {
            archived?: boolean | null;
            color?: string | null;
            force?: boolean;
            pinned?: boolean | null;
            title?: string | null;
        };
        AuthView: {
            logout_url?: string | null;
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
            ws: components["schemas"]["WsView"];
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
        ContinueOutcome: {
            remaining: components["schemas"]["PauseReason"][];
            removed: components["schemas"]["PauseReason"][];
        };
        ConversationDetail: components["schemas"]["AttributeView"] & {
            activity: components["schemas"]["ActivityView"];
            crunch: components["schemas"]["CrunchDetail"];
            effort: string;
            execution?: components["schemas"]["LiveExecutionView"] | null;
            holds: components["schemas"]["HoldView"][];
            last_activity_at?: string | null;
            model: string;
            permission_mode: string;
            queue: components["schemas"]["QueueView"];
            settings: components["schemas"]["SettingsView"];
            /** Format: int32 */
            unread_count: number;
        };
        ConversationSnapshot: {
            detail: components["schemas"]["ConversationDetail"];
            history: components["schemas"]["HistoryPage"];
            open_requests: components["schemas"]["RequestView"][];
            queue: components["schemas"]["SnapshotQueueView"];
            scheduled: components["schemas"]["ScheduledMessage"][];
        };
        Counts: {
            /** Format: int64 */
            all: number;
            /** Format: int64 */
            archived: number;
            groups: components["schemas"]["GroupCount"][];
            /** Format: int64 */
            pinned: number;
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
        EffectiveSettings: {
            effort: string;
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
        ErrorCode: "unauthenticated" | "forbidden_user" | "origin_rejected" | "invalid_request" | "invalid_offset" | "not_found" | "method_not_allowed" | "idempotency_conflict" | "decision_conflict" | "request_not_open" | "decision_invalid" | "resource_limit" | "not_implemented" | "too_many_requests" | "db_unavailable" | "internal" | "queue_full" | "conversation_closed" | "execution_already_live" | "invalid_state" | "not_pending" | "schedule_time_out_of_range" | "too_many_scheduled" | "machine_not_found" | "machine_unavailable" | "capability_missing" | "limits_mismatch" | "harness_unknown" | "model_unknown" | "effort_not_allowed" | "permission_mode_unknown" | "settings_midsession_unsupported" | "profile_unknown" | "workdir_not_allowed" | "group_not_found" | "group_name_taken" | "archive_active" | "attachment_not_ready" | "attachment_not_uploadable" | "attachment_unsupported" | "attachment_unavailable" | "offset_mismatch" | "upload_in_progress" | "checksum_mismatch" | "attachment_conflict";
        /** @description Детали ответа владельцу; содержимое решения не должно попадать в журналы ошибок. */
        ErrorDetails: {
            current_state?: string;
            question_id?: string;
            reason?: string;
            reasons?: components["schemas"]["ArchiveReason"][];
            winner_decision?: components["schemas"]["DecisionBody"];
        };
        ErrorEnvelope: {
            error: components["schemas"]["ErrorBody"];
        };
        /** @enum {string} */
        ErrorScope: "browser" | "auth" | "hub_db" | "delivery" | "worker_disk" | "cli_start" | "adapter_protocol" | "agent_service";
        Evidence: {
            /** Format: int32 */
            background_work?: number | null;
            /** @enum {string} */
            kind: "idle";
            /** Format: date-time */
            last_turn_at?: string | null;
        } | {
            /** @enum {string} */
            kind: "working";
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
        Group: {
            created_at: string;
            /** Format: uuid */
            id: string;
            name: string;
            /** Format: int32 */
            position: number;
        };
        GroupBody: {
            name: string;
        };
        GroupCount: {
            /** Format: int64 */
            count: number;
            /** Format: uuid */
            id: string;
        };
        GroupPatch: {
            name?: string | null;
            /** Format: int32 */
            position?: number | null;
        };
        GroupsBody: {
            group_ids: string[];
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
            usage: components["schemas"]["UsageDay"][];
        };
        HistoryPage: {
            items: components["schemas"]["TimelineItem"][];
            next_after?: string | null;
            next_before?: string | null;
        };
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
        InputContentBlock: {
            text: string;
            /** @enum {string} */
            type: "text";
        } | (components["schemas"]["AttachmentView"] & {
            /** @enum {string} */
            type: "attachment";
        });
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
            /** Format: uuid */
            retry_of?: string | null;
            state: string;
        };
        /** @enum {string} */
        JobState: "pending" | "fired" | "cancelled" | "failed";
        LimitThresholdsView: {
            /** Format: int64 */
            stale_after_secs: number;
            warn_pcts: number[];
            /** Format: int32 */
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
            permission_mode: string;
            queue: components["schemas"]["QueueView"];
            /** Format: int32 */
            unread_count: number;
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
            /** Format: uuid */
            id: string;
            limits_ok: boolean;
            metrics?: components["schemas"]["MetricsView"] | null;
            name: string;
            os?: string | null;
            profiles: components["schemas"]["ProfileView"][];
            sessions: components["schemas"]["SessionView"][];
            warnings: components["schemas"]["MachineWarning"][];
            worker_version?: string | null;
        };
        /** @enum {string} */
        MachineWarning: "disk_usage_high" | "metrics_stale" | "limits_mismatch";
        MachinesSnapshot: {
            items: components["schemas"]["MachineView"][];
        };
        MachinesView: {
            /** Format: int32 */
            disk_warn_pct: number;
            /** Format: int64 */
            metrics_stale_after_secs: number;
        };
        MeView: {
            login: string;
            /** Format: uuid */
            owner_id: string;
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
        ModelUsageView: {
            agent: string;
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
            efforts: string[];
            id: string;
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
        PartView: {
            agent: string;
            /** Format: uuid */
            block_id: string;
            kind: string;
            model: string;
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
        };
        Point: {
            observed_at: string;
            /** Format: double */
            used_pct: number;
            /** Format: int32 */
            used_permille: number;
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
        ProfileView: {
            description: string;
            harnesses: string[];
            name: string;
            title: string;
        };
        ProviderSeries: {
            five_hour: components["schemas"]["Point"][];
            provider: string;
            weekly: components["schemas"]["Point"][];
        };
        ProviderView: {
            five_hour?: components["schemas"]["WindowView"] | null;
            provider: string;
            weekly?: components["schemas"]["WindowView"] | null;
            weekly_reserve_active: boolean;
            /** @description Сброс недели, в окне которой включился резерв. */
            weekly_reserve_resets_at?: string | null;
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
        SessionView: {
            activity_state: string;
            /** Format: uuid */
            conversation_id: string;
            title: string;
        };
        SettingsPatch: {
            effort?: string | null;
            model?: string | null;
            permission_mode?: string | null;
        };
        SettingsView: {
            effective: components["schemas"]["EffectiveSettings"];
            pending: unknown;
            /** Format: int64 */
            rev: number;
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
                detail: components["schemas"]["ConversationDetail"];
                history: components["schemas"]["HistoryPage"];
                open_requests: components["schemas"]["RequestView"][];
                queue: components["schemas"]["SnapshotQueueView"];
                scheduled: components["schemas"]["ScheduledMessage"][];
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
        } | "nothing_to_stop";
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
        TimelineItem: (components["schemas"]["InputView"] & {
            /** @enum {string} */
            kind: "input";
        }) | (components["schemas"]["BlockView"] & {
            /** @enum {string} */
            kind: "block";
        });
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
        WindowView: {
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
            /** @description Содержимое готового вложения, всегда как скачиваемый файл */
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
            /** @description Нет личности */
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
            /** @description Чужой пользователь */
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
    get__api_v1_conversations: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                filter: string | null;
                q: string | null;
                cursor: string | null;
                limit: number | null;
            };
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
    get__api_v1_conversations__id__blocks__block_id_: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                block_id: string;
                offset: number | null;
                limit: number | null;
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
    get__api_v1_conversations__id__history: {
        parameters: {
            query?: {
                before?: string;
                after?: string;
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
            query?: never;
            header?: never;
            path: {
                id: string;
                history_limit: number | null;
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
    get__api_v1_events: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                scope: string;
                after: string;
                limit: number | null;
            };
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
            /** @description Текущие лимиты */
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
            query?: never;
            header?: never;
            path: {
                provider: string | null;
                from: string | null;
                to: string | null;
                tz: string | null;
            };
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
            /** @description Недавние каталоги */
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
            /** @description Нет личности */
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
            /** @description Чужой пользователь */
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
            /** @description Нет личности */
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
            /** @description Чужой пользователь */
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
