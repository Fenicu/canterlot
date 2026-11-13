/** Клиент диктовки: локальные типы контракта, пока сгенерированных ещё нет. */
import { http, type HttpResponse } from "#lib/api/http.js";
import type { DictationStatusView } from "./availability.js";
import type { DictationTerm } from "./terms.js";

export interface DictationTermsView {
  terms: DictationTerm[];
}

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
