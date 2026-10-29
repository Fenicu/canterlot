import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, it, expect } from 'vitest';
import { describeError, type ErrorAction } from '$lib/api/errors';
import { ApiError, AuthRequired, NetworkError } from '$lib/api/http';
import type { components } from '$lib/api/generated/openapi';

type ErrorCode = components['schemas']['ErrorCode'];

const ALL_ERROR_CODES: ErrorCode[] = [
  'unauthenticated',
  'forbidden_user',
  'origin_rejected',
  'invalid_request',
  'invalid_offset',
  'not_found',
  'method_not_allowed',
  'idempotency_conflict',
  'decision_conflict',
  'request_not_open',
  'decision_invalid',
  'resource_limit',
  'not_implemented',
  'too_many_requests',
  'db_unavailable',
  'internal',
  'queue_full',
  'conversation_closed',
  'execution_already_live',
  'invalid_state',
  'not_pending',
  'schedule_time_out_of_range',
  'too_many_scheduled',
  'machine_not_found',
  'machine_unavailable',
  'capability_missing',
  'limits_mismatch',
  'harness_unknown',
  'model_unknown',
  'effort_not_allowed',
  'permission_mode_unknown',
  'settings_midsession_unsupported',
  'profile_unknown',
  'workdir_not_allowed',
  'group_not_found',
  'group_name_taken',
  'archive_active',
  'attachment_not_ready',
  'attachment_not_uploadable',
  'attachment_unsupported',
  'attachment_unavailable',
  'offset_mismatch',
  'upload_in_progress',
  'checksum_mismatch',
  'attachment_conflict',
  'preset_not_found',
  'preset_builtin',
  'preset_revision_conflict',
  'preset_title_taken',
  'preset_limit_reached',
  'preset_invalid'
];

const VALID_ACTIONS: ErrorAction[] = [
  'retry_same_key',
  'new_operation',
  'restore_connection',
  'check_acceptance',
  'stop_process',
  'sign_in_again',
  'reload',
  'none'
];

describe('api/errors', () => {
  it('every_published_error_code_has_a_russian_message', () => {
    const spec = JSON.parse(readFileSync(resolve(process.cwd(), '../docs/api/openapi.json'), 'utf8')) as {
      components: { schemas: { ErrorCode: { enum: string[] } } };
    };
    const codes = spec.components.schemas.ErrorCode.enum;
    expect(new Set(codes)).toEqual(new Set(ALL_ERROR_CODES));
    for (const code of codes) {
      const desc = describeError(new ApiError(400, code as ErrorCode, 'server', 'browser', 'never', 'corr-spec'));
      expect(desc.message).toMatch(/\p{Script=Cyrillic}/u);
      expect(desc.message).not.toContain('Непредвиденная');
    }
  });

  it('error_actions_cover_every_code_in_the_api_error_table', () => {
    expect(ALL_ERROR_CODES.length).toBe(51);

    for (const code of ALL_ERROR_CODES) {
      const err = new ApiError(400, code, 'Server text', 'browser', 'never', 'corr-123');
      const desc = describeError(err);

      expect(VALID_ACTIONS).toContain(desc.action);
      expect(desc.message).toBeTruthy();
      expect(typeof desc.message).toBe('string');
      // Must not be the fallback message
      expect(desc.message).not.toContain('Непредвиденная');
      expect(desc.correlationId).toBe('corr-123');
      if (desc.action === 'none') expect(desc.actionLabel).toBeNull();
      else expect(typeof desc.actionLabel).toBe('string');
    }
  });

  it('handles_unknown_or_generic_errors_with_fallback_and_correlation_id', () => {
    const unknownApiErr = new ApiError(500, 'some_new_code' as ErrorCode, 'msg', 'hub_db', 'never', 'corr-456');
    const desc = describeError(unknownApiErr);
    expect(desc.action).toBe('none');
    expect(desc.actionLabel).toBeNull();
    expect(desc.message).toContain('corr-456');
    expect(desc.correlationId).toBe('corr-456');

    const genericErr = new Error('Random failure');
    const desc2 = describeError(genericErr);
    expect(desc2.action).toBe('none');
    expect(desc2.actionLabel).toBeNull();
    expect(desc2.message).toContain('Непредвиденная');
    expect(desc2.correlationId).toBeUndefined();
  });

  it('error_actions_follow_auth_network_and_the_retry_field', () => {
    const auth = describeError(new AuthRequired('unauthenticated'));
    expect(auth.action).toBe('sign_in_again');
    expect(auth.actionLabel).toBe('Войти снова');

    const offline = describeError(new NetworkError('timeout'));
    expect(offline.action).toBe('restore_connection');

    const unknown = new ApiError(503, 'brand_new' as ErrorCode, 'later', 'hub_db', 'safe_same_key', 'corr-retry');
    const retried = describeError(unknown);
    expect(retried.action).toBe('retry_same_key');
    expect(retried.correlationId).toBe('corr-retry');
  });
});
