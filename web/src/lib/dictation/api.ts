/** Клиент диктовки. Статус описан локально: OpenAPI отдаёт его перечисления простыми строками. */
import { http, type HttpResponse } from "#lib/api/http.js";
import type { components } from "#lib/api/generated/openapi.js";
import type { DictationStatusView } from "./availability.js";
import type { DictationTerm } from "./terms.js";

export type DictationTermsView = components["schemas"]["TermsBody"];

export function getDictationStatus(): Promise<HttpResponse<DictationStatusView>> {
  return http("GET", "/api/v1/dictation/status");
}

export function getDictationTerms(): Promise<HttpResponse<DictationTermsView>> {
  return http("GET", "/api/v1/dictation/terms");
}

// Список заменяется целиком; повтор с тем же ключом хаб не применит дважды.
export function putDictationTerms(
  terms: DictationTerm[],
  key: string,
): Promise<HttpResponse<DictationTermsView>> {
  return http("PUT", "/api/v1/dictation/terms", { terms }, { idempotencyKey: key });
}
