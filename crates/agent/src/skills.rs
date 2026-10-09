//! The Assistant's prompts: a short system prompt and the skills (workflows) it follows. They are
//! plain Markdown in `crates/agent/skills/`, compiled in, so they are reviewed like code and the
//! cached prompt prefix is identical on every call.

use filmcraft_llm::SystemBlock;

/// Who the Assistant is and how it works.
pub const SYSTEM: &str = include_str!("../skills/system.md");
/// Talking-head cleanup: silences, fillers, content cuts, captions.
pub const TALKING_HEAD_CLEANUP: &str = include_str!("../skills/talking_head_cleanup.md");
/// Matching a reference video's treatment.
pub const STYLE_FROM_REFERENCE: &str = include_str!("../skills/style_from_reference.md");

/// Every skill, in a fixed order.
pub const SKILLS: &[&str] = &[TALKING_HEAD_CLEANUP, STYLE_FROM_REFERENCE];

/// The top-level system blocks: the system prompt, then the skills with a cache breakpoint after
/// them (they never change within a conversation).
pub fn system_blocks() -> Vec<SystemBlock> {
    vec![SystemBlock::new(SYSTEM), SystemBlock::cached(SKILLS.join("\n\n"))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_present_and_stable() {
        assert!(SYSTEM.contains("FilmCraft Assistant"));
        assert!(TALKING_HEAD_CLEANUP.contains("find_silences"));
        assert!(STYLE_FROM_REFERENCE.contains("analyze_media"));
        assert_eq!(system_blocks(), system_blocks());
        assert!(system_blocks().last().is_some_and(|b| b.cache));
    }
}
