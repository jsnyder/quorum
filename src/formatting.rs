/// Numeric formatting: human-readable k/M suffixes per DESIGN.md section 11.

use std::time::Duration;

pub fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms >= 4_000 {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{}ms", ms)
    }
}

pub fn format_cost(dollars: f64) -> String {
    format!("${:.2}", dollars)
}

pub fn format_pct(ratio: f64) -> String {
    format!("{}%", (ratio * 100.0).round() as u32)
}

/// Estimate cost in USD given model name and token counts.
/// Prices are per 1M tokens. Fallback for unknown models uses conservative estimates.
pub fn estimate_cost(model: &str, tokens_in: u64, tokens_out: u64) -> f64 {
    let (input_per_m, output_per_m) = model_pricing(model);
    (tokens_in as f64 * input_per_m + tokens_out as f64 * output_per_m) / 1_000_000.0
}

fn model_pricing(model: &str) -> (f64, f64) {
    // (input $/M, output $/M)
    // Prices as of 2026-04-17. Sources:
    //   OpenAI: https://openai.com/api/pricing/
    //   Anthropic: https://docs.anthropic.com/en/docs/about-claude/pricing
    //   Google: https://ai.google.dev/gemini-api/docs/pricing
    match model {
        m if m.starts_with("gpt-5.4") => (2.5, 15.0),
        m if m.starts_with("gpt-5.3") => (1.0, 4.0),
        m if m.starts_with("gpt-5.2") => (1.75, 14.0),
        m if m.starts_with("gpt-4o") => (2.5, 10.0),
        m if m.starts_with("gpt-4.1") => (2.0, 8.0),
        m if m.starts_with("o3") => (2.0, 8.0),
        m if m.starts_with("o4-mini") => (1.1, 4.4),
        m if m.contains("claude-sonnet") => (3.0, 15.0),
        m if m.contains("claude-opus") => (5.0, 25.0),
        m if m.contains("claude-haiku") => (1.0, 5.0),
        m if m.starts_with("gemini-2.5-pro") => (1.25, 2.50),
        m if m.starts_with("gemini-2.5-flash") => (0.10, 0.40),
        _ => (3.0, 15.0), // conservative fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_count_cases() {
        let cases = [
            (0, "0"),
            (1, "1"),
            (999, "999"),
            (1_000, "1.0k"),
            (1_050, "1.1k"),
            (1_500, "1.5k"),
            (10_000, "10.0k"),
            (63_100, "63.1k"),
            (999_999, "1000.0k"),
            (1_000_000, "1.0M"),
            (1_500_000, "1.5M"),
            (42_000_000, "42.0M"),
        ];
        for (input, expected) in cases {
            assert_eq!(format_count(input), expected, "format_count({input})");
        }
    }

    #[test]
    fn format_duration_cases() {
        let cases = [
            (Duration::from_millis(0), "0ms"),
            (Duration::from_millis(50), "50ms"),
            (Duration::from_millis(1318), "1318ms"),
            (Duration::from_secs(4), "4.0s"),
            (Duration::from_millis(4200), "4.2s"),
            (Duration::from_secs(62), "62.0s"),
        ];
        for (input, expected) in cases {
            assert_eq!(format_duration(input), expected, "format_duration({input:?})");
        }
    }

    #[test]
    fn format_cost_cases() {
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(0.005), "$0.01");
        assert_eq!(format_cost(2.14), "$2.14");
        assert_eq!(format_cost(15.7), "$15.70");
    }

    #[test]
    fn format_pct_cases() {
        assert_eq!(format_pct(0.0), "0%");
        assert_eq!(format_pct(0.5), "50%");
        assert_eq!(format_pct(0.888), "89%");
        assert_eq!(format_pct(1.0), "100%");
    }

    #[test]
    fn estimate_cost_known_model() {
        let cost = estimate_cost("gpt-5.4", 1_000_000, 500_000);
        // gpt-5.4: $2.50/M input, $15/M output -> $2.50 + $7.50 = $10.00
        assert!((cost - 10.0).abs() < 0.01, "cost was {cost}");
    }

    #[test]
    fn estimate_cost_unknown_model_fallback() {
        let cost = estimate_cost("unknown-model-xyz", 1_000_000, 500_000);
        // fallback: $3/M input, $15/M output -> $3.00 + $7.50 = $10.50
        assert!((cost - 10.5).abs() < 0.01, "cost was {cost}");
    }

    #[test]
    fn estimate_cost_zero_tokens() {
        assert!((estimate_cost("gpt-5.4", 0, 0)).abs() < f64::EPSILON);
    }

    #[test]
    fn estimate_cost_gemini() {
        let cost = estimate_cost("gemini-2.5-pro", 1_000_000, 500_000);
        // gemini-2.5-pro: $1.25/M input, $2.50/M output -> $1.25 + $1.25 = $2.50
        assert!((cost - 2.50).abs() < 0.01, "cost was {cost}");
    }
}
