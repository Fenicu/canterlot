import { http, type HttpResponse, type RequestOptions } from "#lib/api/http.js";
import type { operations } from "#lib/api/generated/openapi.js";

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

export function listMachines(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_machines"]>>> {
  return http("GET", "/api/v1/machines", undefined, opts);
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

export function conversationBlock(
  conversationId: string,
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

export function listPresets(
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_presets"]>>> {
  return http("GET", "/api/v1/presets", undefined, opts);
}

export function getPreset(
  presetId: string,
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["get__api_v1_presets__id_"]>>> {
  return http("GET", `/api/v1/presets/${id(presetId)}`, undefined, opts);
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
  opts?: RequestOptions,
): Promise<HttpResponse<Ok<operations["patch__api_v1_presets__id_"]>>> {
  return http(
    "PATCH",
    `/api/v1/presets/${id(presetId)}`,
    body,
    keyed(key, opts),
  );
}

export function deletePreset(
  presetId: string,
  key: string,
  opts?: RequestOptions,
): Promise<HttpResponse<void>> {
  return http(
    "DELETE",
    `/api/v1/presets/${id(presetId)}`,
    undefined,
    keyed(key, opts),
  );
}
