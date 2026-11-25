/** Замена `$app/state` в юнит-тестах: состояние страницы без роутера, реактивное, как у SvelteKit. */
export const page = $state<{ state: App.PageState; url: URL }>({ state: {}, url: new URL('http://localhost/') });
