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
            archived: boolean;
            color?: string | null;
            created_at: string;
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
        AuthView: {
            logout_url?: string | null;
        };
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
            version: string;
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
            crunch: components["schemas"]["CrunchDetail"];
            effort: string;
            execution?: components["schemas"]["LiveExecutionView"] | null;
            holds: components["schemas"]["HoldView"][];
            last_activity_at?: string | null;
            model: string;
            permission_mode: string;
            queue: components["schemas"]["QueueView"];
            settings: components["schemas"]["SettingsView"];
            /** @description Принятый ход записан: следующий запуск продолжит беседу, а не начнёт новую. */
            turn_recorded: boolean;
            /** Format: int32 */
            unread_count: number;
        };
        ConversationSnapshot: {
            background: components["schemas"]["BackgroundTaskView"][];
            context?: components["schemas"]["ContextSnapshot"] | null;
            detail: components["schemas"]["ConversationDetail"];
            history: components["schemas"]["HistoryPage"];
            /** @description Незавершённые запросы по времени создания: открытые, с сохранённым и с передаваемым агенту решением. */
            open_requests: components["schemas"]["RequestView"][];
            queue: components["schemas"]["SnapshotQueueView"];
            scheduled: components["schemas"]["ScheduledMessage"][];
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
        ErrorCode: "unauthenticated" | "forbidden_user" | "origin_rejected" | "invalid_request" | "invalid_offset" | "not_found" | "method_not_allowed" | "idempotency_conflict" | "decision_conflict" | "request_not_open" | "decision_invalid" | "resource_limit" | "not_implemented" | "too_many_requests" | "db_unavailable" | "internal" | "queue_full" | "conversation_closed" | "execution_already_live" | "invalid_state" | "not_pending" | "schedule_time_out_of_range" | "too_many_scheduled" | "machine_not_found" | "machine_unavailable" | "capability_missing" | "limits_mismatch" | "harness_unknown" | "model_unknown" | "effort_not_allowed" | "permission_mode_unknown" | "settings_midsession_unsupported" | "profile_unknown" | "workdir_not_allowed" | "group_not_found" | "group_name_taken" | "archive_active" | "conversation_deleted" | "conversation_live" | "attachment_not_ready" | "attachment_not_uploadable" | "attachment_unsupported" | "attachment_unavailable" | "offset_mismatch" | "upload_in_progress" | "checksum_mismatch" | "attachment_conflict" | "preset_not_found" | "preset_builtin" | "preset_revision_conflict" | "preset_title_taken" | "preset_limit_reached" | "preset_invalid" | "spawn_whitelist_invalid" | "not_delegated" | "worktree_removed" | "worktree_not_removed";
        /** @description Детали ответа владельцу; содержимое решения не должно попадать в журналы ошибок. */
        ErrorDetails: {
            /**
             * Format: int64
             * @description Текущая версия пресета (`preset_revision_conflict`).
             */
            current_revision?: number;
            current_state?: string;
            /** @description Поле запроса, которое не прошло проверку (`preset_invalid`). */
            field?: string;
            question_id?: string;
            reason?: components["schemas"]["ErrorReason"];
            reasons?: components["schemas"]["ArchiveReason"][];
            winner_decision?: components["schemas"]["DecisionBody"];
        };
        ErrorEnvelope: {
            error: components["schemas"]["ErrorBody"];
        };
        /** @enum {string} */
        ErrorReason: "closed_without_decision" | "generation_advanced" | "invalid_idempotency_key" | "input_not_queued" | "no_running_turn" | "request_open" | "compacting" | "unsupported_harness" | "capability_missing" | "revision_conflict" | "steer_in_flight" | "empty_content" | "content_too_long" | "attachment_unavailable" | "attachment_not_ready" | "attachment_not_found" | "run_in_progress" | "no_live_execution";
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
            /** @description Медианы скорости ответа по моделям; Codex не передаёт времени генерации и сюда не попадает. */
            speed: components["schemas"]["ModelSpeedSummary"][];
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
            answer?: string | null;
            /** Format: uuid */
            answer_block_id?: string | null;
            /** Format: int64 */
            answer_bytes?: number | null;
            branch?: string | null;
            branch_kept?: boolean | null;
            /** Format: int64 */
            changed_files?: number | null;
            /** Format: uuid */
            child_id?: string | null;
            /** @description `accepted`: событие ушло другому подписчику, а не принявшему родителю. */
            closed?: boolean | null;
            commits?: components["schemas"]["HubCommit"][] | null;
            cpu_busy?: boolean | null;
            /** @description `approval`: `allowed`, `denied`, `failed` или `lapsed`. */
            decision?: string | null;
            detail?: string | null;
            /** Format: int64 */
            duration_ms?: number | null;
            /**
             * @description `session_result`, `asked`, `stalled`, `failed`, `quota_paused`, `stopped`,
             *     `accepted`, `approval` или `deleted`.
             */
            kind: string;
            /** Format: date-time */
            last_event_at?: string | null;
            outcome?: string | null;
            phase?: string | null;
            process_alive?: boolean | null;
            provider?: string | null;
            question?: string | null;
            reason?: string | null;
            /** Format: uuid */
            request_id?: string | null;
            /** Format: uuid */
            session_id?: string | null;
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
            limits_ok: boolean;
            metrics?: components["schemas"]["MetricsView"] | null;
            name: string;
            os?: string | null;
            sessions: components["schemas"]["SessionView"][];
            warnings: components["schemas"]["MachineWarning"][];
            worker_version?: string | null;
        };
        /** @enum {string} */
        MachineWarning: "disk_usage_high" | "metrics_stale" | "limits_mismatch" | "cj_journal_stopped" | "sj_journal_stopped";
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
            /**
             * Format: int64
             * @description Окно контекста модели в токенах, если харнесс его знает.
             */
            context_window?: number | null;
            efforts: string[];
            id: string;
            /** @description Модель принимает картинки; `null`: у харнесса это не свойство модели. */
            images?: boolean | null;
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
        } | {
            compaction?: components["schemas"]["CompactionProgress"] | null;
            /** @enum {string} */
            reason: "session_stuck";
        };
        Point: {
            observed_at: string;
            /** Format: double */
            used_pct: number;
            /** Format: int32 */
            used_permille: number;
        };
        /** @description Детальный ответ: текст только у пользовательских пресетов. */
        Preset: components["schemas"]["PresetSummary"] & {
            text?: string | null;
        };
        PresetBody: {
            description?: string | null;
            text: string;
            title: string;
        };
        /**
         * @description Вид пресета в списках и событиях.
         * @enum {string}
         */
        PresetKind: "clean" | "owner" | "custom";
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
        /** @description Строка списка, события и снимка: текста пресета здесь нет. */
        PresetSummary: {
            created: components["schemas"]["PresetOrigin"];
            description: string;
            /** @description `clean`, `owner` или UUID пользовательского пресета. */
            id: string;
            kind: components["schemas"]["PresetKind"];
            last_edit?: components["schemas"]["PresetOrigin"] | null;
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
        ProviderSeries: {
            five_hour: components["schemas"]["Point"][];
            provider: string;
            weekly: components["schemas"]["Point"][];
        };
        ProviderView: {
            five_hour?: components["schemas"]["WindowView"] | null;
            /** @description У провайдера нет пятичасового окна (например, план без этого лимита): счётчик скрывается. */
            five_hour_absent: boolean;
            monthly?: components["schemas"]["WindowView"] | null;
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
        RefreshView: {
            /** @description Провайдеры, чьим машинам отправлена просьба об опросе. */
            requested: string[];
            /** Format: int64 */
            retry_after_secs: number;
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
        SessionView: {
            activity_state: string;
            /** Format: uuid */
            conversation_id: string;
            title: string;
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
        SnapshotQueueView: {
            hold_reasons: components["schemas"]["HoldView"][];
            pause_reasons: components["schemas"]["PauseReason"][];
            queued: components["schemas"]["InputView"][];
            run_slot: components["schemas"]["RunSlot"];
        };
        Snapshot_ConversationSnapshot: {
            cursor: string;
            data: {
                background: components["schemas"]["BackgroundTaskView"][];
                context?: components["schemas"]["ContextSnapshot"] | null;
                detail: components["schemas"]["ConversationDetail"];
                history: components["schemas"]["HistoryPage"];
                /** @description Незавершённые запросы по времени создания: открытые, с сохранённым и с передаваемым агенту решением. */
                open_requests: components["schemas"]["RequestView"][];
                queue: components["schemas"]["SnapshotQueueView"];
                scheduled: components["schemas"]["ScheduledMessage"][];
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
    get__api_v1_presets: {
        parameters: {
            query?: never;
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
            query?: never;
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
