import { onOwnerRetired } from '$lib/storage/owner';

const notices = new Map<string, string>();
onOwnerRetired(() => notices.clear());
export function leaveAttachmentNotice(conversationId: string, message: string): void { notices.set(conversationId, message); }
export function takeAttachmentNotice(conversationId: string): string | null {
  const message = notices.get(conversationId) ?? null;
  notices.delete(conversationId);
  return message;
}
