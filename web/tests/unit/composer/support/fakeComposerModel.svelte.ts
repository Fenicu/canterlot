/**
 * Модель поля ввода без сети и черновиков: тестам раскладки нужен только реактивный текст.
 * Отправки записываются в `calls`, чтобы проверять, какая кнопка сработала.
 */
export class FakeComposerModel {
	text = $state('');
	errorMessage = $state<string | null>(null);
	sending = $state(false);
	draftApplied = $state(0);
	calls: string[] = [];
	/** Текст поля на каждый вызов `typed()`: по нему видно, что в общий черновик ушла очистка. */
	typedTexts: string[] = [];

	get gate(): { ok: boolean; reason: string } {
		return this.text.trim() ? { ok: true, reason: '' } : { ok: false, reason: 'Пустое сообщение' };
	}

	typed(): void {
		this.typedTexts.push(this.text);
	}
	attachmentsChanged(): void {}
	takeBack(): void {}
	remoteDraft(): void {}
	attach(): void {}
	startDraft(): Promise<void> {
		return Promise.resolve();
	}
	flushDraft(): Promise<void> {
		return Promise.resolve();
	}
	leaveDraft(): Promise<void> {
		return Promise.resolve();
	}
	send(): Promise<void> {
		this.calls.push('send');
		return Promise.resolve();
	}
	sendSteer(): Promise<void> {
		this.calls.push('steer');
		return Promise.resolve();
	}
	sendAsGoal(): Promise<void> {
		this.calls.push('goal');
		return Promise.resolve();
	}
	sendLater(): Promise<void> {
		this.calls.push('later');
		return Promise.resolve();
	}
}
