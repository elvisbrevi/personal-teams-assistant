//! The name a session this host starts carries in the provider's session list (Claude's Remote
//! Control, Codex's thread name), so the person can tell several apart. It is built only here,
//! from a fixed purpose and the work item's ID and title the host already holds; it travels as
//! a field of the provider's protocol, never as a command-line argument, and is never logged.

/// Every name starts with this, so none can read as an option or a path.
const PREFIX: &str = "Teams Assistant";
/// The longest name, in characters, within what the providers' session lists show.
pub(crate) const NAME_CHARS: usize = 100;

/// A session's name: `Teams Assistant`, or `Teams Assistant - <what it is about>` without
/// control or invisible formatting characters and shortened to `NAME_CHARS` on a character
/// boundary.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SessionName(String);
impl Default for SessionName {
    fn default() -> Self {
        Self(PREFIX.into())
    }
}
impl SessionName {
    /// A coding or planning run of a work item: `Teams Assistant - 42 Fix rounding`.
    pub(crate) fn item(id: u64, title: &str) -> Self {
        Self::about(&format!("{id} {title}"))
    }
    /// A session for a fixed purpose, about an item when it has one:
    /// `Teams Assistant - director 42`.
    pub(crate) fn purpose(purpose: &str, item: Option<u64>) -> Self {
        match item {
            Some(id) => Self::about(&format!("{purpose} {id}")),
            None => Self::about(purpose),
        }
    }
    /// A request run writing a new work item of `kind` (`hu`, `bug` or `epic`).
    pub(crate) fn request(kind: &str) -> Self {
        Self::purpose(
            match kind {
                "hu" => "new user story",
                "bug" => "new bug",
                "epic" => "new epic",
                _ => "new work item",
            },
            None,
        )
    }
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
    /// The prefix and `detail`, its whitespace and control characters each one space, runs of
    /// them collapsed, invisible formatting characters removed.
    fn about(detail: &str) -> Self {
        let mut clean = String::new();
        for c in detail.chars() {
            if c.is_whitespace() || c.is_control() {
                if !clean.is_empty() && !clean.ends_with(' ') {
                    clean.push(' ');
                }
            } else if !invisible(c) {
                clean.push(c);
            }
        }
        let clean = clean.trim_end();
        if clean.is_empty() {
            return Self::default();
        }
        let name = format!("{PREFIX} - {clean}");
        if name.chars().count() <= NAME_CHARS {
            return Self(name);
        }
        let mut short: String = name.chars().take(NAME_CHARS - 1).collect();
        short.truncate(short.trim_end().len());
        short.push('…');
        Self(short)
    }
}
/// Zero-width and bidirectional formatting characters, which could hide or reorder the text.
fn invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_name_carries_the_item_shortened_without_control_or_invisible_characters() {
        assert_eq!(
            SessionName::item(4211, "Fix rounding").as_str(),
            "Teams Assistant - 4211 Fix rounding"
        );
        // Line breaks, tabs and quotes: one line, quotes kept as text.
        let name = SessionName::item(
            7,
            "  First line\r\n\tsecond \"quoted\" 'part'\u{0}\u{1b}[31m\n",
        );
        assert_eq!(
            name.as_str(),
            "Teams Assistant - 7 First line second \"quoted\" 'part' [31m"
        );
        // A title that reads as an option or a path stays inside the name.
        let name = SessionName::item(8, "--dangerously-skip-permissions /etc/passwd");
        assert!(name.as_str().starts_with("Teams Assistant - 8 --"));
        // Invisible reordering characters are removed.
        assert_eq!(
            SessionName::item(9, "a\u{202E}b\u{200B}c\u{FEFF}").as_str(),
            "Teams Assistant - 9 abc"
        );
        // 300 characters, multibyte ones included: shortened on a character boundary.
        let title = "ñ".repeat(150) + &"x".repeat(150);
        let name = SessionName::item(10, &title);
        assert_eq!(name.as_str().chars().count(), NAME_CHARS);
        assert!(name.as_str().ends_with("ñ…"));
        assert!(name.as_str().starts_with("Teams Assistant - 10 ñ"));
        // A space at the cut is not kept before the ellipsis.
        let name = SessionName::item(11, &("x".repeat(77) + " " + &"y".repeat(200)));
        assert!(name.as_str().ends_with("x…"), "{}", name.as_str());
        assert_eq!(name.as_str().chars().count(), NAME_CHARS - 1);
        assert!(name.as_str().chars().all(|c| !c.is_control()));
    }
    #[test]
    fn a_session_without_an_item_keeps_a_fixed_name() {
        assert_eq!(SessionName::default().as_str(), "Teams Assistant");
        assert_eq!(
            SessionName::item(12, "\n\t ").as_str(),
            "Teams Assistant - 12"
        );
        assert_eq!(
            SessionName::purpose("director", Some(42)).as_str(),
            "Teams Assistant - director 42"
        );
        use super::super::DirectTask;
        for (task, name) in [
            (DirectTask::Classify, "Teams Assistant - director 42"),
            (DirectTask::Summarize, "Teams Assistant - summary 42"),
            (DirectTask::Supervise, "Teams Assistant - supervision 42"),
        ] {
            assert_eq!(task.session_name(Some(42)).as_str(), name);
        }
        assert_eq!(
            SessionName::purpose("director", None).as_str(),
            "Teams Assistant - director"
        );
        assert_eq!(
            SessionName::request("hu").as_str(),
            "Teams Assistant - new user story"
        );
        assert_eq!(
            SessionName::request("bug").as_str(),
            "Teams Assistant - new bug"
        );
        assert_eq!(
            SessionName::request("epic").as_str(),
            "Teams Assistant - new epic"
        );
    }
}
