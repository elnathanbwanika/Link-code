//! The context meter.
//!
//! Link Code always shows how much of the model's window is in use, and what is
//! taking up the space. Token counts are *estimates* produced by
//! [`estimate_tokens`] unless the provider returned exact usage numbers, in
//! which case [`ContextMeter::set`] is fed the real values.

use serde::{Deserialize, Serialize};

/// Rough token estimate: ~4 characters per token, plus a small allowance per
/// line. Deliberately conservative (it over-estimates slightly) so the
/// auto-compaction trigger fires early rather than too late.
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let chars = text.chars().count();
    let lines = text.lines().count();
    (chars / 4) + (lines / 2) + 1
}

/// The buckets shown in the context breakdown popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPart {
    SystemPrompt,
    ProjectMemory,
    Files,
    History,
    ToolResults,
}

impl ContextPart {
    pub const ALL: [ContextPart; 5] = [
        ContextPart::SystemPrompt,
        ContextPart::ProjectMemory,
        ContextPart::Files,
        ContextPart::History,
        ContextPart::ToolResults,
    ];

    pub const COUNT: usize = 5;

    pub fn index(self) -> usize {
        match self {
            ContextPart::SystemPrompt => 0,
            ContextPart::ProjectMemory => 1,
            ContextPart::Files => 2,
            ContextPart::History => 3,
            ContextPart::ToolResults => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ContextPart::SystemPrompt => "System prompt",
            ContextPart::ProjectMemory => "Project memory",
            ContextPart::Files => "Files in context",
            ContextPart::History => "Conversation",
            ContextPart::ToolResults => "Tool results",
        }
    }

    /// Stable CSS class used by the breakdown bars.
    pub fn css_class(self) -> &'static str {
        match self {
            ContextPart::SystemPrompt => "ctx-system",
            ContextPart::ProjectMemory => "ctx-memory",
            ContextPart::Files => "ctx-files",
            ContextPart::History => "ctx-history",
            ContextPart::ToolResults => "ctx-tools",
        }
    }
}

/// A snapshot of context usage, ready to render.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContextBreakdown {
    pub system_prompt: usize,
    pub project_memory: usize,
    pub files: usize,
    pub history: usize,
    pub tool_results: usize,
    pub total: usize,
    pub max_tokens: usize,
}

impl ContextBreakdown {
    /// Share of the window in use, clamped to `0.0 ..= 1.0`.
    pub fn fraction(&self) -> f64 {
        if self.max_tokens == 0 {
            0.0
        } else {
            (self.total as f64 / self.max_tokens as f64).clamp(0.0, 1.0)
        }
    }

    pub fn percent(&self) -> u32 {
        (self.fraction() * 100.0).round() as u32
    }

    pub fn remaining(&self) -> usize {
        self.max_tokens.saturating_sub(self.total)
    }

    pub fn needs_compaction(&self, threshold: f64) -> bool {
        self.fraction() >= threshold.clamp(0.0, 1.0)
    }

    /// `14.2k / 32k` — the exact format used in the status bar.
    pub fn compact_label(&self) -> String {
        format!("{} / {}", human_tokens(self.total), human_tokens(self.max_tokens))
    }

    pub fn part(&self, part: ContextPart) -> usize {
        match part {
            ContextPart::SystemPrompt => self.system_prompt,
            ContextPart::ProjectMemory => self.project_memory,
            ContextPart::Files => self.files,
            ContextPart::History => self.history,
            ContextPart::ToolResults => self.tool_results,
        }
    }
}

/// `14200` -> `14.2k`
pub fn human_tokens(tokens: usize) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 1_000_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    }
}

/// Tracks usage per [`ContextPart`].
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMeter {
    max_tokens: usize,
    parts: [usize; ContextPart::COUNT],
    /// Set once a provider reported real usage numbers for the last request.
    exact: bool,
}

impl ContextMeter {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens,
            parts: [0; ContextPart::COUNT],
            exact: false,
        }
    }

    pub fn max_tokens(&self) -> usize {
        self.max_tokens
    }

    pub fn set_max_tokens(&mut self, max_tokens: usize) {
        self.max_tokens = max_tokens;
    }

    /// True when the numbers came from the provider rather than the estimator.
    pub fn is_exact(&self) -> bool {
        self.exact
    }

    /// Overwrite one bucket with an exact count.
    pub fn set(&mut self, part: ContextPart, tokens: usize) {
        self.parts[part.index()] = tokens;
    }

    /// Overwrite one bucket using the estimator.
    pub fn set_text(&mut self, part: ContextPart, text: &str) {
        self.set(part, estimate_tokens(text));
    }

    /// Add to one bucket (used while streaming tool results in).
    pub fn add(&mut self, part: ContextPart, tokens: usize) {
        self.parts[part.index()] += tokens;
    }

    /// Record a provider-reported total for the whole request.
    ///
    /// The provider's `prompt_tokens` covers *everything* that was sent, so the
    /// non-history buckets are kept as they are and the conversation bucket
    /// absorbs the remainder. That way the breakdown always adds up to exactly
    /// what the provider billed, and the meter agrees with the footer.
    pub fn record_exact_total(&mut self, prompt_tokens: usize, tool_tokens: usize) {
        self.parts[ContextPart::ToolResults.index()] = tool_tokens;
        let non_history = self.parts[ContextPart::SystemPrompt.index()]
            + self.parts[ContextPart::ProjectMemory.index()]
            + self.parts[ContextPart::Files.index()];
        self.parts[ContextPart::History.index()] =
            prompt_tokens.saturating_sub(tool_tokens.saturating_add(non_history));
        self.exact = true;
    }

    pub fn total(&self) -> usize {
        self.parts.iter().sum()
    }

    pub fn breakdown(&self) -> ContextBreakdown {
        ContextBreakdown {
            system_prompt: self.parts[ContextPart::SystemPrompt.index()],
            project_memory: self.parts[ContextPart::ProjectMemory.index()],
            files: self.parts[ContextPart::Files.index()],
            history: self.parts[ContextPart::History.index()],
            tool_results: self.parts[ContextPart::ToolResults.index()],
            total: self.total(),
            max_tokens: self.max_tokens,
        }
    }

    pub fn fraction(&self) -> f64 {
        self.breakdown().fraction()
    }

    pub fn needs_compaction(&self, threshold: f64) -> bool {
        self.breakdown().needs_compaction(threshold)
    }

    pub fn reset(&mut self) {
        self.parts = [0; ContextPart::COUNT];
        self.exact = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimator_is_monotonic_and_nonzero() {
        assert_eq!(estimate_tokens(""), 0);
        let short = estimate_tokens("hello world");
        let long = estimate_tokens(&"hello world ".repeat(50));
        assert!(short > 0);
        assert!(long > short);
    }

    #[test]
    fn breakdown_totals_and_percentage() {
        let mut meter = ContextMeter::new(1_000);
        meter.set(ContextPart::SystemPrompt, 100);
        meter.set(ContextPart::History, 750);
        let breakdown = meter.breakdown();
        assert_eq!(breakdown.total, 850);
        assert_eq!(breakdown.percent(), 85);
        assert!(breakdown.needs_compaction(0.85));
        assert!(!breakdown.needs_compaction(0.9));
        assert_eq!(breakdown.remaining(), 150);
        assert_eq!(breakdown.compact_label(), "850 / 1.0k");
    }

    #[test]
    fn records_exact_provider_usage() {
        let mut meter = ContextMeter::new(32_000);
        meter.set(ContextPart::SystemPrompt, 500);
        meter.record_exact_total(5_000, 1_200);
        assert!(meter.is_exact());
        let breakdown = meter.breakdown();
        assert_eq!(breakdown.tool_results, 1_200);
        assert_eq!(breakdown.total, 5_000);
    }

    #[test]
    fn zero_window_never_divides_by_zero() {
        let meter = ContextMeter::new(0);
        assert_eq!(meter.fraction(), 0.0);
        assert_eq!(meter.breakdown().percent(), 0);
    }

    #[test]
    fn human_tokens_formats_all_magnitudes() {
        assert_eq!(human_tokens(950), "950");
        assert_eq!(human_tokens(14_200), "14.2k");
        assert_eq!(human_tokens(2_500_000), "2.5M");
    }

    #[test]
    fn reset_clears_everything() {
        let mut meter = ContextMeter::new(100);
        meter.set(ContextPart::Files, 50);
        meter.reset();
        assert_eq!(meter.total(), 0);
        assert!(!meter.is_exact());
    }
}
