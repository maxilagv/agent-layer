//! Reading a task's spending: hints that point at where it could cost less.
use layeragent_config::{Config, ModelTeamAccess, Wire};
use serde_json::Value;

/// Input below this many tokens is too little to judge caching by.
const CACHE_JUDGED_FROM: f64 = 50_000.0;
/// Below this share of cached input a model's caching is called poor.
const POOR_CACHE: f64 = 0.3;

/// Plain-language hints from a usage report (see
/// `Storage::usage_report`) and the configuration that priced it. Each hint
/// names what to change; none is applied automatically.
pub fn cost_hints(report: &Value, config: &Config) -> Vec<String> {
    let mut hints = Vec::new();
    let total = report["total"]["cost_usd"].as_f64().unwrap_or(0.0);
    let models = report["by_model"].as_array().cloned().unwrap_or_default();
    for row in &models {
        let alias = row["model"].as_str().unwrap_or("");
        let input = row["input_tokens"].as_f64().unwrap_or(0.0);
        let ratio = row["cache_read_ratio"].as_f64().unwrap_or(0.0);
        let configured = config.models.get(alias);
        if input >= CACHE_JUDGED_FROM && ratio < POOR_CACHE {
            let anthropic = configured
                .and_then(|m| config.providers.get(&m.provider))
                .is_some_and(|p| p.wire == Wire::Anthropic);
            if anthropic && configured.is_some_and(|m| !m.prompt_cache) {
                hints.push(format!(
                    "Almost none of {alias}'s input was cached: set prompt_cache = true for it; Anthropic caches only marked prefixes."
                ));
            } else {
                hints.push(format!(
                    "Only {:.0}% of {alias}'s input came from the provider's cache; stable instructions and fewer changes to early context help it.",
                    ratio * 100.0
                ));
            }
        }
        if let Some(model) = configured {
            let output_cost = row["output_tokens"].as_f64().unwrap_or(0.0)
                * model.output_per_million.unwrap_or(0.0)
                / 1_000_000.0;
            let cost = row["cost_usd"].as_f64().unwrap_or(0.0);
            if cost > 0.0 && output_cost / cost > 0.6 && cost >= total * 0.25 {
                hints.push(format!(
                    "Output is most of {alias}'s cost: ask for shorter answers or a lower effort level."
                ));
            }
        }
    }
    // The model that cost most making many short calls is doing work a
    // cheaper model could do.
    if let Some(top) = models.first() {
        let alias = top["model"].as_str().unwrap_or("");
        let calls = top["calls"].as_f64().unwrap_or(0.0);
        let short = top["short_calls"].as_f64().unwrap_or(0.0);
        let price = config
            .models
            .get(alias)
            .and_then(|m| m.input_per_million)
            .unwrap_or(0.0);
        let cheaper = config
            .models
            .iter()
            .filter(|(other, m)| {
                other.as_str() != alias
                    && m.team_access != ModelTeamAccess::None
                    && config.require_model_egress(other).is_ok()
                    && m.input_per_million.is_some_and(|p| p * 3.0 <= price)
            })
            .min_by(|a, b| {
                a.1.input_per_million
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&b.1.input_per_million.unwrap_or(f64::INFINITY))
            })
            .map(|(other, _)| other.clone());
        if calls >= 10.0
            && short / calls >= 0.6
            && let Some(cheaper) = cheaper
        {
            hints.push(format!(
                "{alias} made {short:.0} short calls out of {calls:.0}; reading, searching and summarizing could go to {cheaper} with spawn_agent."
            ));
        }
    }
    let children: f64 = report["by_depth"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["depth"].as_i64().unwrap_or(0) > 0)
        .filter_map(|row| row["cost_usd"].as_f64())
        .sum();
    if total > 0.0 && children / total > 0.5 {
        hints.push(format!(
            "Child agents spent {:.0}% of the task: give them cheaper models or narrower tasks.",
            children / total * 100.0
        ));
    }
    if let Some(budget) = report["budget_usd"].as_f64()
        && total >= budget * 0.95
    {
        hints.push(
            "The task used its whole budget: plan first (it costs little), then build the agreed scope."
                .into(),
        );
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> Config {
        serde_json::from_value(json!({
            "primary": "opus",
            "providers": {
                "anthropic": {"wire":"anthropic","base_url":"https://api.anthropic.com/v1"},
                "deepseek": {"wire":"openai_chat","base_url":"https://api.deepseek.com"}
            },
            "models": {
                "opus": {"provider":"anthropic","id":"claude-opus","context_window":200000,"input_per_million":5.0,"output_per_million":25.0},
                "deepseek": {"provider":"deepseek","id":"deepseek-chat","context_window":64000,"input_per_million":0.3,"output_per_million":1.1}
            }
        }))
        .expect("config")
    }

    #[test]
    fn hints_name_the_change_that_would_cost_less() {
        let report = json!({
            "budget_usd": 2.0,
            "total": {"cost_usd": 2.0},
            "by_model": [
                {"model":"opus","calls":20,"short_calls":15,"input_tokens":400000,"output_tokens":4000,"cache_read_ratio":0.05,"cost_usd":1.6},
                {"model":"deepseek","calls":4,"short_calls":1,"input_tokens":20000,"output_tokens":2000,"cache_read_ratio":0.0,"cost_usd":0.4}
            ],
            "by_depth": [{"depth":0,"cost_usd":0.6},{"depth":1,"cost_usd":1.4}]
        });
        let hints = cost_hints(&report, &config());
        let all = hints.join("\n");
        assert!(all.contains("set prompt_cache = true for it"), "{all}");
        assert!(
            all.contains("could go to deepseek with spawn_agent"),
            "{all}"
        );
        assert!(all.contains("Child agents spent 70%"), "{all}");
        assert!(all.contains("used its whole budget"), "{all}");
        assert!(
            !all.contains("deepseek's input"),
            "too little input to judge: {all}"
        );

        let mut cached = config();
        cached.models.get_mut("opus").expect("opus").prompt_cache = true;
        let healthy = json!({
            "total": {"cost_usd": 1.0},
            "by_model": [{"model":"opus","calls":3,"short_calls":0,"input_tokens":400000,"output_tokens":1000,"cache_read_ratio":0.8,"cost_usd":1.0}],
            "by_depth": [{"depth":0,"cost_usd":1.0}]
        });
        assert!(cost_hints(&healthy, &cached).is_empty());
    }
}
