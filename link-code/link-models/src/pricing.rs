//! Cost estimation.
//!
//! Local models are free, so cost is zero for any endpoint that is not a known
//! cloud provider. Prices are in USD per million tokens and are only used for
//! the footer estimate on a response block — never for billing.

use crate::registry::ProviderKind;
use crate::types::Usage;

/// `(substring of the model id, input $/Mtok, output $/Mtok)`
const PRICES: &[(&str, f64, f64)] = &[
    ("gpt-4o-mini", 0.15, 0.60),
    ("gpt-4o", 2.50, 10.00),
    ("gpt-4.1-mini", 0.40, 1.60),
    ("gpt-4.1", 2.00, 8.00),
    ("o4-mini", 1.10, 4.40),
    ("o3-mini", 1.10, 4.40),
    ("claude-3-5-haiku", 0.80, 4.00),
    ("claude-3-5-sonnet", 3.00, 15.00),
    ("claude-3-7-sonnet", 3.00, 15.00),
    ("claude-sonnet-4", 3.00, 15.00),
    ("claude-opus-4", 15.00, 75.00),
    ("gemini-1.5-flash", 0.075, 0.30),
    ("gemini-1.5-pro", 1.25, 5.00),
    ("gemini-2.0-flash", 0.10, 0.40),
    ("gemini-2.5-pro", 1.25, 10.00),
    ("deepseek-chat", 0.27, 1.10),
    ("deepseek-reasoner", 0.55, 2.19),
    ("llama-3.3-70b", 0.59, 0.79),
    ("mistral-large", 2.00, 6.00),
];

/// Estimated USD cost of one request. Zero for local endpoints.
pub fn estimate_cost(provider: ProviderKind, model: &str, usage: Usage) -> f64 {
    if !provider.is_cloud() {
        return 0.0;
    }
    let lowered = model.to_ascii_lowercase();
    // Longest match wins, so `gpt-4o-mini` is not priced as `gpt-4o`.
    let matched = PRICES
        .iter()
        .filter(|(needle, _, _)| lowered.contains(needle))
        .max_by_key(|(needle, _, _)| needle.len());
    let Some((_, input, output)) = matched else {
        return 0.0;
    };
    let million = 1_000_000.0;
    (usage.prompt_tokens as f64 / million) * input + (usage.completion_tokens as f64 / million) * output
}

/// `$0.0023`, or `free` for local models.
pub fn format_cost(provider: ProviderKind, cost: f64) -> String {
    if !provider.is_cloud() {
        return "free (local)".to_string();
    }
    if cost <= 0.0 {
        return "$0.00".to_string();
    }
    if cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.3}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_models_are_free() {
        let usage = Usage {
            prompt_tokens: 100_000,
            completion_tokens: 100_000,
        };
        assert_eq!(estimate_cost(ProviderKind::Ollama, "qwen2.5-coder:32b", usage), 0.0);
        assert_eq!(format_cost(ProviderKind::Ollama, 0.0), "free (local)");
    }

    #[test]
    fn cloud_models_are_priced_with_longest_match() {
        let usage = Usage {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
        };
        // gpt-4o-mini must not be priced as gpt-4o.
        let cost = estimate_cost(ProviderKind::OpenAiCompatible, "openai/gpt-4o-mini", usage);
        assert!((cost - 0.75).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn unknown_models_cost_nothing_rather_than_guessing() {
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 1000,
        };
        assert_eq!(estimate_cost(ProviderKind::OpenAiCompatible, "mystery-model", usage), 0.0);
    }

    #[test]
    fn cost_formatting_scales() {
        assert_eq!(format_cost(ProviderKind::Anthropic, 0.0), "$0.00");
        assert_eq!(format_cost(ProviderKind::Anthropic, 0.00042), "$0.0004");
        assert_eq!(format_cost(ProviderKind::Anthropic, 0.1234), "$0.123");
    }
}
