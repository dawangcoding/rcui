/// Internal content prefixes used by Claude to mark system messages.
/// Messages with these prefixes should be filtered from user-facing history.
const INTERNAL_CONTENT_PREFIXES: &[&str] = &[
    "<environment_details>",
    "<system>",
    "[SYSTEM]",
    "System prompt:",
];

/// Check if a message content starts with an internal prefix.
pub fn is_internal_content(content: &str) -> bool {
    let trimmed = content.trim();
    INTERNAL_CONTENT_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}
