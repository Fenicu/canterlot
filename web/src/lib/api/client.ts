import { http, type HttpResponse, type RequestOptions } from "#lib/api/http.js";
import type { operations } from "#lib/api/generated/openapi.js";
import type { DraftRecord } from "#lib/drafts/sync.js";

type Ok<T> = T extends { responses: infer R }
  ? R extends { 200: { content: { "application/json": infer B } } }
    ? B
    : R extends { 201: { content: { "application/json": infer B } } }
      ? B
      : R extends { 202: { content: { "application/json": infer B } } }
        ? B
        : void
  : void;

type Body<T> = T extends {
  requestBody?: { content: { "application/json": infer B } };
}
  ? B
  : undefined;

type Query = Record<string, string | number | boolean | null | undefined>;

function withQuery(path: string, query?: Query): string {
  if (!query) return path;
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined && value !== null) params.set(key, String(value));
  }
  const text = params.toString();
  return text ? `${path}?${text}` : path;
}

function id(value: string): string {
  return encodeURIComponent(value);
}

function keyed(key: string, opts?: RequestOptions): RequestOptions {
  return { ...opts, idempotencyKey: key };
}

export function me(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_me"]>>> {
  return http("GET", "/api/v1/me", undefined, opts);
}

export function getConfig(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_config"]>>> {
  return http("GET", "/api/v1/config", undefined, opts);
}

export function snapshotConversations(
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_snapshot_conversations"]>>> {
  return http(
    "GET",
    withQuery("/api/v1/snapshot/conversations", query),
    undefined,
    opts,
  );
}

export function snapshotMachines(
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_snapshot_machines"]>>> {
  return http(
    "GET",
    withQuery("/api/v1/snapshot/machines", query),
    undefined,
    opts,
  );
}

export function conversationSnapshot(
  conversationId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__snapshot"]>>
> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}/snapshot`,
    undefined,
    opts,
  );
}

export function getConversationDraft(
  conversationId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<DraftRecord>> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}/draft`,
    undefined,
    opts,
  );
}

export function putConversationDraft(
  conversationId: string,
  body: { text: string; attachment_ids: string[]; base_revision: number; device_id: string },
  opts?: RequestOptions,
): Promise<HttpResponse<DraftRecord>> {
  return http(
    "PUT",
    `/api/v1/conversations/${id(conversationId)}/draft`,
    body,
    opts,
  );
}

export function listConversations(
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_conversations"]>>> {
  return http(
    "GET",
    withQuery("/api/v1/conversations", query),
    undefined,
    opts,
  );
}

export function createConversation(
  body: Body<operations["post__api_v1_conversations"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_conversations"]>>> {
  return http("POST", "/api/v1/conversations", body, keyed(key, opts));
}

export function getConversation(
  conversationId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_conversations__id_"]>>> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}`,
    undefined,
    opts,
  );
}

export function patchConversation(
  conversationId: string,
  body: Body<operations["patch__api_v1_conversations__id_"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["patch__api_v1_conversations__id_"]>>> {
  return http(
    "PATCH",
    `/api/v1/conversations/${id(conversationId)}`,
    body,
    keyed(key, opts),
  );
}

export function getSpawnWhitelist(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_spawn_whitelist"]>>> {
  return http("GET", "/api/v1/spawn-whitelist", undefined, opts);
}

// entries: null возвращает встроенный список.
export function putSpawnWhitelist(
  body: Body<operations["put__api_v1_spawn_whitelist"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["put__api_v1_spawn_whitelist"]>>> {
  return http("PUT", "/api/v1/spawn-whitelist", body, keyed(key, opts));
}

export function getLimitSettings(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_limit_settings"]>>> {
  return http("GET", "/api/v1/limit-settings", undefined, opts);
}

// settings: null возвращает значения по умолчанию.
export function putLimitSettings(
  body: Body<operations["put__api_v1_limit_settings"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["put__api_v1_limit_settings"]>>> {
  return http("PUT", "/api/v1/limit-settings", body, keyed(key, opts));
}

// Команды на месте (`job_run`): список, задача с хвостом, журнал и отмена.
export function listJobs(
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_jobs"]>>> {
  return http("GET", withQuery("/api/v1/jobs", query), undefined, opts);
}

export function getJob(
  jobId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_jobs__id_"]>>> {
  return http("GET", `/api/v1/jobs/${id(jobId)}`, undefined, opts);
}

export function getJobLog(
  jobId: string,
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_jobs__id__log"]>>> {
  return http(
    "GET",
    withQuery(`/api/v1/jobs/${id(jobId)}/log`, query),
    undefined,
    opts,
  );
}

export function cancelJob(
  jobId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_jobs__id__cancel"]>>> {
  return http("POST", `/api/v1/jobs/${id(jobId)}/cancel`, undefined, keyed(key, opts));
}

// Выключатель удалённой работы машины: принимает ли она делегатов и команды других машин.
export function putMachineRemoteWork(
  machineId: string,
  body: Body<operations["put__api_v1_machines__id__remote_work"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["put__api_v1_machines__id__remote_work"]>>> {
  return http(
    "PUT",
    `/api/v1/machines/${id(machineId)}/remote-work`,
    body,
    keyed(key, opts),
  );
}

export function getPromptSuggestionSettings(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_prompt_suggestion_settings"]>>> {
  return http("GET", "/api/v1/prompt-suggestion-settings", undefined, opts);
}

export function putPromptSuggestionSettings(
  body: Body<operations["put__api_v1_prompt_suggestion_settings"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["put__api_v1_prompt_suggestion_settings"]>>> {
  return http("PUT", "/api/v1/prompt-suggestion-settings", body, keyed(key, opts));
}

export function getClaudeAutoMemorySettings(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_claude_auto_memory_settings"]>>> {
  return http("GET", "/api/v1/claude-auto-memory-settings", undefined, opts);
}

export function putClaudeAutoMemorySettings(
  body: Body<operations["put__api_v1_claude_auto_memory_settings"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["put__api_v1_claude_auto_memory_settings"]>>> {
  return http("PUT", "/api/v1/claude-auto-memory-settings", body, keyed(key, opts));
}

export function getHibernateSettings(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_hibernate_settings"]>>> {
  return http("GET", "/api/v1/hibernate-settings", undefined, opts);
}

export function putHibernateSettings(
  body: Body<operations["put__api_v1_hibernate_settings"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["put__api_v1_hibernate_settings"]>>> {
  return http("PUT", "/api/v1/hibernate-settings", body, keyed(key, opts));
}

export function listGroups(
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_groups"]>>> {
  return http("GET", withQuery("/api/v1/groups", query), undefined, opts);
}

export function createGroup(
  body: Body<operations["post__api_v1_groups"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_groups"]>>> {
  return http("POST", "/api/v1/groups", body, keyed(key, opts));
}

export function patchGroup(
  groupId: string,
  body: Body<operations["patch__api_v1_groups__id_"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["patch__api_v1_groups__id_"]>>> {
  return http("PATCH", `/api/v1/groups/${id(groupId)}`, body, keyed(key, opts));
}

export function deleteGroup(
  groupId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["delete__api_v1_groups__id_"]>>> {
  return http(
    "DELETE",
    `/api/v1/groups/${id(groupId)}`,
    undefined,
    keyed(key, opts),
  );
}

export function deleteConversation(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["delete__api_v1_conversations__id_"]>>> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}`,
    undefined,
    keyed(key, opts),
  );
}

export function restoreConversation(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_conversations__id__restore"]>>> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/restore`,
    undefined,
    keyed(key, opts),
  );
}

export function purgeConversation(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["delete__api_v1_conversations__id__trash"]>>> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}/trash`,
    undefined,
    keyed(key, opts),
  );
}

export function listMachines(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_machines"]>>> {
  return http("GET", "/api/v1/machines", undefined, opts);
}

/** Лента обновлений CLI и моделей, свежие первыми; курсор — из `next_cursor`. */
export function listUpdates(
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_updates"]>>> {
  return http("GET", withQuery("/api/v1/updates", query), undefined, opts);
}

export function getMachine(
  machineId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_machines__id_"]>>> {
  return http("GET", `/api/v1/machines/${id(machineId)}`, undefined, opts);
}

export function suggestWorkdirs(
  machineId: string,
  path: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_machines__id__workdirs_suggest"]>>
> {
  return http(
    "GET",
    withQuery(`/api/v1/machines/${id(machineId)}/workdirs/suggest`, { path }),
    undefined,
    opts,
  );
}

export function claudeHistories(
  machineId: string,
  workdir: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_machines__id__claude_histories"]>>
> {
  return http(
    "GET",
    withQuery(`/api/v1/machines/${id(machineId)}/claude-histories`, {
      workdir,
    }),
    undefined,
    opts,
  );
}

export function recentWorkdirs(
  machineId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_machines__id__workdirs_recent"]>>
> {
  return http(
    "GET",
    `/api/v1/machines/${id(machineId)}/workdirs/recent`,
    undefined,
    opts,
  );
}

export function getOwnerOverview(
  machineId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_machines__id__owner_instructions"]>>
> {
  return http(
    "GET",
    `/api/v1/machines/${id(machineId)}/owner-instructions`,
    undefined,
    opts,
  );
}

export function getOwnerFile(
  machineId: string,
  query: { kind: string; project?: string; name?: string },
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_machines__id__owner_instructions_file"]>>
> {
  return http(
    "GET",
    withQuery(`/api/v1/machines/${id(machineId)}/owner-instructions/file`, query),
    undefined,
    opts,
  );
}

export function getLimits(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_limits"]>>> {
  return http("GET", "/api/v1/limits", undefined, opts);
}

export function refreshLimits(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_limits_refresh"]>>> {
  return http("POST", "/api/v1/limits/refresh", undefined, opts);
}

export function limitsHistory(
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_limits_history"]>>> {
  return http(
    "GET",
    withQuery("/api/v1/limits/history", query),
    undefined,
    opts,
  );
}

export function listScheduled(
  conversationId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__scheduled"]>>
> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}/scheduled`,
    undefined,
    opts,
  );
}

export function createScheduled(
  conversationId: string,
  body: Body<operations["post__api_v1_conversations__id__scheduled"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__scheduled"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/scheduled`,
    body,
    keyed(key, opts),
  );
}

export function deleteScheduled(
  scheduledId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["delete__api_v1_scheduled__id_"]>>> {
  return http(
    "DELETE",
    `/api/v1/scheduled/${id(scheduledId)}`,
    undefined,
    keyed(key, opts),
  );
}

export function deleteAlarm(
  conversationId: string,
  alarmId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<
      operations["delete__api_v1_conversations__id__alarms__alarm_id_"]
    >
  >
> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}/alarms/${id(alarmId)}`,
    undefined,
    keyed(key, opts),
  );
}

export function createAttachment(
  body: Body<operations["post__api_v1_attachments"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_attachments"]>>> {
  return http("POST", "/api/v1/attachments", body, keyed(key, opts));
}

export function getAttachment(
  attachmentId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_attachments__id_"]>>> {
  return http(
    "GET",
    `/api/v1/attachments/${id(attachmentId)}`,
    undefined,
    opts,
  );
}

export function deleteAttachment(
  attachmentId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["delete__api_v1_attachments__id_"]>>> {
  return http(
    "DELETE",
    `/api/v1/attachments/${id(attachmentId)}`,
    undefined,
    keyed(key, opts),
  );
}

export function completeAttachment(
  attachmentId: string,
  body: Body<operations["post__api_v1_attachments__id__complete"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_attachments__id__complete"]>>
> {
  return http(
    "POST",
    `/api/v1/attachments/${id(attachmentId)}/complete`,
    body,
    keyed(key, opts),
  );
}

export function headAttachmentContent(
  attachmentId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<void>> {
  return http(
    "HEAD",
    `/api/v1/attachments/${id(attachmentId)}/content`,
    undefined,
    opts,
  );
}

export function conversationHistory(
  conversationId: string,
  query?: Query,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__history"]>>
> {
  return http(
    "GET",
    withQuery(`/api/v1/conversations/${id(conversationId)}/history`, query),
    undefined,
    opts,
  );
}

export function conversationInputs(
  conversationId: string,
  query?: Query,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__inputs"]>>
> {
  return http(
    "GET",
    withQuery(`/api/v1/conversations/${id(conversationId)}/inputs`, query),
    undefined,
    opts,
  );
}

export function createSideChat(
  conversationId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__side"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/side`,
    undefined,
    keyed(crypto.randomUUID(), opts),
  );
}

export function deleteSideChat(
  conversationId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["delete__api_v1_conversations__id__side"]>>
> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}/side`,
    undefined,
    keyed(crypto.randomUUID(), opts),
  );
}

export function conversationImages(
  conversationId: string,
  query?: Query,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__images"]>>
> {
  return http(
    "GET",
    withQuery(`/api/v1/conversations/${id(conversationId)}/images`, query),
    undefined,
    opts,
  );
}

export function conversationBlock(  conversationId: string,
  blockId: string,
  query?: Query,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["get__api_v1_conversations__id__blocks__block_id_"]>
  >
> {
  return http(
    "GET",
    withQuery(
      `/api/v1/conversations/${id(conversationId)}/blocks/${id(blockId)}`,
      query,
    ),
    undefined,
    opts,
  );
}

export function getRun(
  conversationId: string,
  runId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__runs__run_id_"]>>
> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}/runs/${id(runId)}`,
    undefined,
    opts,
  );
}

export function conversationUsage(
  conversationId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__usage"]>>
> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}/usage`,
    undefined,
    opts,
  );
}

export function delegation(
  conversationId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__delegation"]>>
> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}/delegation`,
    undefined,
    opts,
  );
}

export function conversationLane(
  conversationId: string,
  blockId: string,
  query?: Query,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__lanes__block_id_"]>>
> {
  return http(
    "GET",
    withQuery(
      `/api/v1/conversations/${id(conversationId)}/lanes/${id(blockId)}`,
      query,
    ),
    undefined,
    opts,
  );
}

export function eventsRange(
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_events"]>>> {
  return http("GET", withQuery("/api/v1/events", query), undefined, opts);
}

export function postInput(
  conversationId: string,
  body: Body<operations["post__api_v1_conversations__id__inputs"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__inputs"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/inputs`,
    body,
    keyed(key, opts),
  );
}

export function cancelInput(
  inputId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_inputs__id__cancel"]>>> {
  return http(
    "POST",
    `/api/v1/inputs/${id(inputId)}/cancel`,
    undefined,
    keyed(key, opts),
  );
}

export function markRead(
  conversationId: string,
  body: Body<operations["post__api_v1_conversations__id__read"]>,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__read"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/read`,
    body,
    opts,
  );
}

export function interrupt(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__interrupt"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/interrupt`,
    undefined,
    keyed(key, opts),
  );
}

/** Состояние карточки «Ход завис»: `open` или причина, по которой она закрыта. */
export function hungEpisode(
  conversationId: string,
  episodeId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["get__api_v1_conversations__id__hung_episodes__episode_id_"]>
  >
> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}/hung-episodes/${id(episodeId)}`,
    undefined,
    opts,
  );
}

export function hungInterrupt(
  conversationId: string,
  episodeId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["post__api_v1_conversations__id__hung_episodes__episode_id__interrupt"]>
  >
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/hung-episodes/${id(episodeId)}/interrupt`,
    undefined,
    keyed(key, opts),
  );
}

/** «Завершить» на карточке рутины: ответ 204, повтор безвреден. */
export function dismissRoutineReport(
  conversationId: string,
  reportId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["post__api_v1_conversations__id__routine_reports__report_id__dismiss"]>
  >
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/routine-reports/${id(reportId)}/dismiss`,
    undefined,
    keyed(key, opts),
  );
}

export function hungSteer(
  conversationId: string,
  episodeId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["post__api_v1_conversations__id__hung_episodes__episode_id__steer"]>
  >
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/hung-episodes/${id(episodeId)}/steer`,
    undefined,
    keyed(key, opts),
  );
}

export function sendNow(
  conversationId: string,
  inputId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["post__api_v1_conversations__id__inputs__input_id__send_now"]>
  >
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/inputs/${id(inputId)}/send-now`,
    undefined,
    keyed(key, opts),
  );
}

export function editInput(
  conversationId: string,
  inputId: string,
  body: Body<operations["patch__api_v1_conversations__id__inputs__input_id_"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["patch__api_v1_conversations__id__inputs__input_id_"]>>
> {
  return http(
    "PATCH",
    `/api/v1/conversations/${id(conversationId)}/inputs/${id(inputId)}`,
    body,
    keyed(key, opts),
  );
}

export function accept(
  conversationId: string,
  body: Body<operations["post__api_v1_conversations__id__accept"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__accept"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/accept`,
    body,
    keyed(key, opts),
  );
}

export function resume(
  conversationId: string,
  body: Body<operations["post__api_v1_conversations__id__resume"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__resume"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/resume`,
    body,
    keyed(key, opts),
  );
}

export function stop(
  conversationId: string,
  body: Body<operations["post__api_v1_conversations__id__stop"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__stop"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/stop`,
    body,
    keyed(key, opts),
  );
}

export function setGoal(
  conversationId: string,
  body: Body<operations["post__api_v1_conversations__id__goal"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_conversations__id__goal"]>>> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/goal`,
    body,
    keyed(key, opts),
  );
}

export function resolveOwnerLost(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["post__api_v1_conversations__id__resolve_owner_lost"]>
  >
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/resolve-owner-lost`,
    undefined,
    keyed(key, opts),
  );
}

export function compact(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__compact"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/compact`,
    undefined,
    keyed(key, opts),
  );
}

export function continueQueue(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__queue_continue"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/queue/continue`,
    undefined,
    keyed(key, opts),
  );
}

export type DecisionBody = Body<
  operations["post__api_v1_conversations__id__requests__request_id__decision"]
>;

export function decideRequest(
  conversationId: string,
  requestId: string,
  body: DecisionBody,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<
      operations["post__api_v1_conversations__id__requests__request_id__decision"]
    >
  >
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/requests/${id(requestId)}/decision`,
    body,
    keyed(key, opts),
  );
}

export function patchSettings(
  conversationId: string,
  body: Body<operations["patch__api_v1_conversations__id__settings"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["patch__api_v1_conversations__id__settings"]>>
> {
  return http(
    "PATCH",
    `/api/v1/conversations/${id(conversationId)}/settings`,
    body,
    keyed(key, opts),
  );
}

export function setCrunch(
  conversationId: string,
  body: Body<operations["put__api_v1_conversations__id__crunch"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["put__api_v1_conversations__id__crunch"]>>
> {
  return http(
    "PUT",
    `/api/v1/conversations/${id(conversationId)}/crunch`,
    body,
    keyed(key, opts),
  );
}

export function setSnooze(
  conversationId: string,
  body: Body<operations["put__api_v1_conversations__id__snooze"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["put__api_v1_conversations__id__snooze"]>>
> {
  return http(
    "PUT",
    `/api/v1/conversations/${id(conversationId)}/snooze`,
    body,
    keyed(key, opts),
  );
}

export function clearSnooze(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["delete__api_v1_conversations__id__snooze"]>>
> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}/snooze`,
    undefined,
    keyed(key, opts),
  );
}

export function setConversationGroups(
  conversationId: string,
  body: Body<operations["put__api_v1_conversations__id__groups"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["put__api_v1_conversations__id__groups"]>>
> {
  return http(
    "PUT",
    `/api/v1/conversations/${id(conversationId)}/groups`,
    body,
    keyed(key, opts),
  );
}

export function pinConversationInGroup(
  conversationId: string,
  groupId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["put__api_v1_conversations__id__groups__group_id__pin"]>
  >
> {
  return http(
    "PUT",
    `/api/v1/conversations/${id(conversationId)}/groups/${id(groupId)}/pin`,
    undefined,
    keyed(key, opts),
  );
}

export function unpinConversationInGroup(
  conversationId: string,
  groupId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["delete__api_v1_conversations__id__groups__group_id__pin"]>
  >
> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}/groups/${id(groupId)}/pin`,
    undefined,
    keyed(key, opts),
  );
}

export function patchScheduled(
  scheduledId: string,
  body: Body<operations["patch__api_v1_scheduled__id_"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["patch__api_v1_scheduled__id_"]>>> {
  return http(
    "PATCH",
    `/api/v1/scheduled/${id(scheduledId)}`,
    body,
    keyed(key, opts),
  );
}

export function getMedia(
  mediaId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<ArrayBuffer>> {
  return http("GET", `/api/v1/media/${id(mediaId)}`, undefined, opts);
}

export function getAttachmentContent(
  attachmentId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<ArrayBuffer>> {
  return http(
    "GET",
    `/api/v1/attachments/${id(attachmentId)}/content`,
    undefined,
    opts,
  );
}

export function resumeAtReset(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__auto_resume"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/auto-resume`,
    undefined,
    keyed(key, opts),
  );
}

export function cancelResumeAtReset(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["delete__api_v1_conversations__id__auto_resume"]>>
> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}/auto-resume`,
    undefined,
    keyed(key, opts),
  );
}

export function spendReserve(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["post__api_v1_conversations__id__spend_reserve"]>>
> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/spend-reserve`,
    undefined,
    keyed(key, opts),
  );
}

export type PresetPurpose = "instructions" | "nudge";

export function listPresets(
  purpose: PresetPurpose = "instructions",
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_presets"]>>> {
  return http(
    "GET",
    withQuery("/api/v1/presets", { purpose }),
    undefined,
    opts,
  );
}

export function getPreset(
  presetId: string,
  purpose: PresetPurpose = "instructions",
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_presets__id_"]>>> {
  return http(
    "GET",
    withQuery(`/api/v1/presets/${id(presetId)}`, { purpose }),
    undefined,
    opts,
  );
}

export function createPreset(
  body: Body<operations["post__api_v1_presets"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_presets"]>>> {
  return http("POST", "/api/v1/presets", body, keyed(key, opts));
}

export function patchPreset(
  presetId: string,
  body: Body<operations["patch__api_v1_presets__id_"]>,
  key: string,
  purpose: PresetPurpose = "instructions",
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["patch__api_v1_presets__id_"]>>> {
  return http(
    "PATCH",
    withQuery(`/api/v1/presets/${id(presetId)}`, { purpose }),
    body,
    keyed(key, opts),
  );
}

export function deletePreset(
  presetId: string,
  key: string,
  purpose: PresetPurpose = "instructions",
  opts?: RequestOptions,
): Promise<HttpResponse<void>> {
  return http(
    "DELETE",
    withQuery(`/api/v1/presets/${id(presetId)}`, { purpose }),
    undefined,
    keyed(key, opts),
  );
}

export function getNudge(
  conversationId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["get__api_v1_conversations__id__nudge"]>>
> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}/nudge`,
    undefined,
    opts,
  );
}

export function putNudge(
  conversationId: string,
  body: Body<operations["put__api_v1_conversations__id__nudge"]>,
  key: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<Ok<operations["put__api_v1_conversations__id__nudge"]>>
> {
  return http(
    "PUT",
    `/api/v1/conversations/${id(conversationId)}/nudge`,
    body,
    keyed(key, opts),
  );
}

export function deleteNudge(
  conversationId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<void>> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}/nudge`,
    undefined,
    keyed(key, opts),
  );
}

export function getRoutingPolicy(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_routing_policy"]>>> {
  return http("GET", "/api/v1/routing-policy", undefined, opts);
}

// policy: null сбрасывает ко встроенной; устаревшая revision даёт 409 routing_policy_conflict.
export function putRoutingPolicy(
  body: Body<operations["put__api_v1_routing_policy"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["put__api_v1_routing_policy"]>>> {
  return http("PUT", "/api/v1/routing-policy", body, keyed(key, opts));
}

export function previewRouting(
  body: Body<operations["post__api_v1_routing_policy_preview"]>,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_routing_policy_preview"]>>> {
  return http("POST", "/api/v1/routing-policy/preview", body, opts);
}

export function getSpawnQueue(
  conversationId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_conversations__id__spawn_queue"]>>> {
  return http("GET", `/api/v1/conversations/${id(conversationId)}/spawn-queue`, undefined, opts);
}

export function cancelSpawnQueue(
  conversationId: string,
  queueId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["delete__api_v1_conversations__id__spawn_queue__queue_id_"]>>> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}/spawn-queue/${id(queueId)}`,
    undefined,
    keyed(key, opts),
  );
}

// Снимает частоту, занятость, лимиты и страж, но не белый список и не режим разрешений.
export function startSpawnQueueNow(
  conversationId: string,
  queueId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_conversations__id__spawn_queue__queue_id__start_now"]>>> {
  return http(
    "POST",
    `/api/v1/conversations/${id(conversationId)}/spawn-queue/${id(queueId)}/start-now`,
    undefined,
    keyed(key, opts),
  );
}

/** План оркестратора с коммитами, числом изменённых файлов и отчётом исполнителей. */
export function getPlan(
  orchestratorId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_plan"]>>> {
  return http("GET", `/api/v1/plan?orchestrator=${id(orchestratorId)}`, undefined, opts);
}

// Запрос поиска идёт только в теле: адрес с ним попал бы в журналы прокси.
export function searchHistory(
  body: Body<operations["post__api_v1_search"]>,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_search"]>>> {
  return http("POST", "/api/v1/search", body, opts);
}

/** Все закладки разговора в порядке ленты. */
export function listConversationBookmarks(
  conversationId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["get__api_v1_conversations__id__bookmarks"]>
  >
> {
  return http(
    "GET",
    `/api/v1/conversations/${id(conversationId)}/bookmarks`,
    undefined,
    opts,
  );
}

/** Поставить закладку: 201 — новая, 200 — уже была. */
export function putBookmark(
  conversationId: string,
  kind: "input" | "block",
  targetId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["put__api_v1_conversations__id__bookmarks__kind___target_id_"]>
  >
> {
  return http(
    "PUT",
    `/api/v1/conversations/${id(conversationId)}/bookmarks/${kind}/${id(targetId)}`,
    undefined,
    opts,
  );
}

/** Снять закладку; повтор безвреден. */
export function deleteBookmark(
  conversationId: string,
  kind: "input" | "block",
  targetId: string,
  opts?: RequestOptions,
): Promise<
  HttpResponse<
    Ok<operations["delete__api_v1_conversations__id__bookmarks__kind___target_id_"]>
  >
> {
  return http(
    "DELETE",
    `/api/v1/conversations/${id(conversationId)}/bookmarks/${kind}/${id(targetId)}`,
    undefined,
    opts,
  );
}

/** Избранное владельца по всем разговорам, новые закладки первыми. */
export function listBookmarks(
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_bookmarks"]>>> {
  return http("GET", withQuery("/api/v1/bookmarks", query), undefined, opts);
}

/** Заготовки сообщений владельца в одной области: global или project с ключом workdir. */
export function listMessageSnippets(
  scope: "global" | "project" = "global",
  projectKey = "",
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_message_snippets"]>>> {
  return http(
    "GET",
    withQuery("/api/v1/message-snippets", { scope, project_key: projectKey }),
    undefined,
    opts,
  );
}

export function getMessageSnippet(
  snippetId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_message_snippets__id_"]>>> {
  return http(
    "GET",
    `/api/v1/message-snippets/${id(snippetId)}`,
    undefined,
    opts,
  );
}

export function createMessageSnippet(
  body: Body<operations["post__api_v1_message_snippets"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_message_snippets"]>>> {
  return http("POST", "/api/v1/message-snippets", body, keyed(key, opts));
}

export function patchMessageSnippet(
  snippetId: string,
  body: Body<operations["patch__api_v1_message_snippets__id_"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["patch__api_v1_message_snippets__id_"]>>> {
  return http(
    "PATCH",
    `/api/v1/message-snippets/${id(snippetId)}`,
    body,
    keyed(key, opts),
  );
}

export function deleteMessageSnippet(
  snippetId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<void>> {
  return http(
    "DELETE",
    `/api/v1/message-snippets/${id(snippetId)}`,
    undefined,
    keyed(key, opts),
  );
}

export function reorderMessageSnippets(
  body: Body<operations["put__api_v1_message_snippets_order"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["put__api_v1_message_snippets_order"]>>> {
  return http("PUT", "/api/v1/message-snippets/order", body, keyed(key, opts));
}

/** Расписания владельца. */
export function listSchedules(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_schedules"]>>> {
  return http("GET", "/api/v1/schedules", undefined, opts);
}

export function createSchedule(
  body: Body<operations["post__api_v1_schedules"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_schedules"]>>> {
  return http("POST", "/api/v1/schedules", body, keyed(key, opts));
}

/** Следующие срабатывания для редактора: пояс передаётся в теле. */
export function previewSchedule(
  body: Body<operations["post__api_v1_schedules_preview"]>,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_schedules_preview"]>>> {
  return http("POST", "/api/v1/schedules/preview", body, opts);
}

/** Умолчания редактора: пояс владельца из настроек пушей. */
export function getScheduleDefaults(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_schedules_defaults"]>>> {
  return http("GET", "/api/v1/schedules/defaults", undefined, opts);
}

export function getSchedule(
  scheduleId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_schedules__id_"]>>> {
  return http("GET", `/api/v1/schedules/${id(scheduleId)}`, undefined, opts);
}

export function patchSchedule(
  scheduleId: string,
  body: Body<operations["patch__api_v1_schedules__id_"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["patch__api_v1_schedules__id_"]>>> {
  return http(
    "PATCH",
    `/api/v1/schedules/${id(scheduleId)}`,
    body,
    keyed(key, opts),
  );
}

export function deleteSchedule(
  scheduleId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["delete__api_v1_schedules__id_"]>>> {
  return http(
    "DELETE",
    `/api/v1/schedules/${id(scheduleId)}`,
    undefined,
    keyed(key, opts),
  );
}

export function pauseSchedule(
  scheduleId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_schedules__id__pause"]>>> {
  return http(
    "POST",
    `/api/v1/schedules/${id(scheduleId)}/pause`,
    undefined,
    keyed(key, opts),
  );
}

export function resumeSchedule(
  scheduleId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_schedules__id__resume"]>>> {
  return http(
    "POST",
    `/api/v1/schedules/${id(scheduleId)}/resume`,
    undefined,
    keyed(key, opts),
  );
}

/** `run_anyway` обходит проверки расписания; ключ защищает от повторной постановки в очередь. */
export function runSchedule(
  scheduleId: string,
  body: Body<operations["post__api_v1_schedules__id__run"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_schedules__id__run"]>>> {
  return http(
    "POST",
    `/api/v1/schedules/${id(scheduleId)}/run`,
    body,
    keyed(key, opts),
  );
}

export function scheduleRuns(
  scheduleId: string,
  query?: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_schedules__id__runs"]>>> {
  return http(
    "GET",
    withQuery(`/api/v1/schedules/${id(scheduleId)}/runs`, query),
    undefined,
    opts,
  );
}

/**
 * Общая память. Хаб не требует `Idempotency-Key` (повтор отсекают версия записи и тема),
 * но общий слой запросов требует ключ у любого изменения, поэтому ключ уходит как есть.
 */
export function listMemory(
  query: Query,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_memory"]>>> {
  return http("GET", withQuery("/api/v1/memory", query), undefined, opts);
}

export function getMemory(
  entryId: string,
  domain: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_memory__id_"]>>> {
  return http(
    "GET",
    withQuery(`/api/v1/memory/${id(entryId)}`, { domain }),
    undefined,
    opts,
  );
}

export function createMemory(
  body: Body<operations["post__api_v1_memory"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_memory"]>>> {
  return http("POST", "/api/v1/memory", body, keyed(key, opts));
}

export function updateMemory(
  entryId: string,
  body: Body<operations["put__api_v1_memory__id_"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["put__api_v1_memory__id_"]>>> {
  return http("PUT", `/api/v1/memory/${id(entryId)}`, body, keyed(key, opts));
}

export function archiveMemory(
  entryId: string,
  body: Body<operations["post__api_v1_memory__id__archive"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_memory__id__archive"]>>> {
  return http(
    "POST",
    `/api/v1/memory/${id(entryId)}/archive`,
    body,
    keyed(key, opts),
  );
}

export function restoreMemory(
  entryId: string,
  body: Body<operations["post__api_v1_memory__id__restore"]>,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["post__api_v1_memory__id__restore"]>>> {
  return http(
    "POST",
    `/api/v1/memory/${id(entryId)}/restore`,
    body,
    keyed(key, opts),
  );
}

/** Удаление навсегда: записи и её прошлых версий больше нет. */
export function deleteMemory(
  entryId: string,
  domain: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["delete__api_v1_memory__id_"]>>> {
  return http(
    "DELETE",
    withQuery(`/api/v1/memory/${id(entryId)}`, { domain }),
    undefined,
    keyed(key, opts),
  );
}

export function memoryRevisions(
  entryId: string,
  domain: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_memory__id__revisions"]>>> {
  return http(
    "GET",
    withQuery(`/api/v1/memory/${id(entryId)}/revisions`, { domain }),
    undefined,
    opts,
  );
}
