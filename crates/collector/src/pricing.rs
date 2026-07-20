//! Cost estimation for a Session, from its token counts and model (C5).
//!
//! The number is an **estimate**: it multiplies the session's cumulative input /
//! output tokens by the model's *public list price*, so the board can label it
//! "est." and a subscription user can hide it (a Max/Pro plan pays no marginal
//! per-token cost, so the estimate would mislead — that suppression is a UI
//! concern; here we only compute the list-price figure).
//!
//! Prices are per **million** tokens in USD and matched by model-family substring,
//! because model ids drift (`claude-opus-4-8`, `gpt-5.6-sol`, …) far faster than
//! their family pricing. An unrecognised model (or the synthetic API-error model)
//! yields `None` rather than a fabricated price, so the card simply shows no cost.

/// Public list price for one model family, per million tokens (USD).
struct Price {
    input_per_mtok: f64,
    output_per_mtok: f64,
}

/// Estimate the list-price cost of a session in USD, or `None` when the model is
/// unknown/absent. `tokens_in`/`tokens_out` are the session's cumulative counts.
pub fn estimate_cost_usd(model: Option<&str>, tokens_in: u64, tokens_out: u64) -> Option<f64> {
    let price = price_for(model?)?;
    Some(
        tokens_in as f64 / 1_000_000.0 * price.input_per_mtok
            + tokens_out as f64 / 1_000_000.0 * price.output_per_mtok,
    )
}

/// Public list price for a model id, matched by family substring. Returns `None`
/// for families we do not price (leaving the card cost-less) rather than guessing.
/// Figures are approximate public list prices and are only ever shown as "est.".
fn price_for(model: &str) -> Option<Price> {
    let m = model.to_ascii_lowercase();

    // OpenAI / Codex (gpt-5 family, o-series). Order matters: the cheaper
    // mini/nano tiers are substrings-of a `gpt-5` id, so test them first.
    if m.contains("gpt-5") || m.starts_with("o3") || m.starts_with("o4") {
        if m.contains("nano") {
            return Some(Price { input_per_mtok: 0.05, output_per_mtok: 0.40 });
        }
        if m.contains("mini") {
            return Some(Price { input_per_mtok: 0.25, output_per_mtok: 2.00 });
        }
        return Some(Price { input_per_mtok: 1.25, output_per_mtok: 10.00 });
    }

    // Anthropic / Claude Code.
    if m.contains("opus") {
        return Some(Price { input_per_mtok: 15.00, output_per_mtok: 75.00 });
    }
    if m.contains("sonnet") {
        return Some(Price { input_per_mtok: 3.00, output_per_mtok: 15.00 });
    }
    if m.contains("haiku") {
        return Some(Price { input_per_mtok: 0.80, output_per_mtok: 4.00 });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_claude_families_by_substring() {
        // 1M in + 1M out of Opus = 15 + 75.
        let c = estimate_cost_usd(Some("claude-opus-4-8"), 1_000_000, 1_000_000).unwrap();
        assert!((c - 90.0).abs() < 1e-9);

        // Sonnet: 3 + 15 per 1M/1M.
        let c = estimate_cost_usd(Some("claude-sonnet-4-5"), 1_000_000, 1_000_000).unwrap();
        assert!((c - 18.0).abs() < 1e-9);
    }

    #[test]
    fn prices_codex_gpt5_and_its_cheaper_tiers() {
        // Base gpt-5: 1.25 in + 10 out.
        let c = estimate_cost_usd(Some("gpt-5.6-sol"), 1_000_000, 1_000_000).unwrap();
        assert!((c - 11.25).abs() < 1e-9);

        // mini tier is matched before the base gpt-5 price.
        let c = estimate_cost_usd(Some("gpt-5-mini"), 1_000_000, 0).unwrap();
        assert!((c - 0.25).abs() < 1e-9);
    }

    #[test]
    fn scales_with_token_counts() {
        // 250k in + 40k out of Sonnet = 0.25*3 + 0.04*15 = 0.75 + 0.60 = 1.35.
        let c = estimate_cost_usd(Some("claude-sonnet-4-5"), 250_000, 40_000).unwrap();
        assert!((c - 1.35).abs() < 1e-9);
    }

    #[test]
    fn unknown_or_missing_model_has_no_cost() {
        assert!(estimate_cost_usd(None, 1000, 100).is_none());
        assert!(estimate_cost_usd(Some("<synthetic>"), 1000, 100).is_none());
        assert!(estimate_cost_usd(Some("some-future-model"), 1000, 100).is_none());
    }
}
