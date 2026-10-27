pub const HARNESS_PROVIDERS: &[(&str, &str)] = &[
    ("claude_code", "claude"),
    ("codex", "codex"),
    ("synthetic", "claude"),
    ("synthetic-exit-on-interrupt", "claude"),
    ("synthetic-codex", "codex"),
];

pub fn provider_of(harness: &str) -> Option<&'static str> {
    HARNESS_PROVIDERS
        .iter()
        .find_map(|&(name, provider)| (name == harness).then_some(provider))
}

pub fn harnesses_of(provider: &str) -> Vec<&'static str> {
    HARNESS_PROVIDERS
        .iter()
        .filter_map(|&(harness, name)| (name == provider).then_some(harness))
        .collect()
}
