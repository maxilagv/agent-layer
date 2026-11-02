use layeragent_config::Config;
use layeragent_protocol::EffortLevel;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EffortPolicy {
    pub(crate) requested: EffortLevel,
    pub(crate) effective: EffortLevel,
}

/// `output_used` is the turn's output so far. Auto escalates before the current
/// level's output share is exhausted, exactly as it does for rounds, so a turn
/// is never stopped below the operator's configured output ceiling.
pub(crate) fn resolve(
    config: &Config,
    requested: EffortLevel,
    completed_rounds: usize,
    context_pressure: bool,
    output_used: u64,
) -> EffortPolicy {
    let effective = if requested == EffortLevel::Auto {
        // Medium allows 2/4 of the configured output and High 3/4 (see `apply`).
        let output_pressure =
            output_used.saturating_mul(4) >= config.max_output_tokens.saturating_mul(3);
        let output_elevated = output_used.saturating_mul(2) >= config.max_output_tokens;
        let pressure = completed_rounds.saturating_mul(8) >= config.max_rounds.saturating_mul(3)
            || output_pressure;
        let elevated = completed_rounds.saturating_mul(4) >= config.max_rounds
            || context_pressure
            || output_elevated;
        if pressure {
            EffortLevel::Xhigh
        } else if elevated {
            EffortLevel::High
        } else {
            EffortLevel::Medium
        }
    } else {
        requested
    };
    EffortPolicy {
        requested,
        effective,
    }
}

pub(crate) fn apply(mut config: Config, policy: EffortPolicy) -> Config {
    let numerator = match policy.effective {
        EffortLevel::Auto | EffortLevel::Medium => 2,
        EffortLevel::Low => 1,
        EffortLevel::High => 3,
        EffortLevel::Xhigh => 4,
    };
    config.max_rounds = (config.max_rounds.saturating_mul(numerator) / 4).max(1);
    config.max_output_tokens =
        (config.max_output_tokens.saturating_mul(numerator as u64) / 4).max(1);
    config.default_effort = policy.effective;
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_escalates_before_its_output_share_is_exhausted() {
        let config = Config {
            max_rounds: 16,
            max_output_tokens: 4_096,
            ..Default::default()
        };
        let at = |used| resolve(&config, EffortLevel::Auto, 1, false, used).effective;
        assert_eq!(at(0), EffortLevel::Medium);
        // Medium's share is 2048: reaching it escalates instead of exhausting the turn.
        assert_eq!(at(2_048), EffortLevel::High);
        assert_eq!(at(3_000), EffortLevel::High);
        assert!(
            apply(
                config.clone(),
                resolve(&config, EffortLevel::Auto, 1, false, 3_000)
            )
            .max_output_tokens
                > 3_000
        );
        assert_eq!(at(3_072), EffortLevel::Xhigh);
        assert_eq!(
            apply(
                config.clone(),
                resolve(&config, EffortLevel::Auto, 1, false, 3_072)
            )
            .max_output_tokens,
            4_096,
            "the configured ceiling is the limit, never exceeded"
        );
        // Explicit levels keep their bounded share.
        assert_eq!(
            resolve(&config, EffortLevel::Medium, 1, false, 3_000).effective,
            EffortLevel::Medium
        );
    }

    #[test]
    fn ceilings_and_bounded_auto_escalation_hold() {
        let config = Config {
            max_rounds: 40,
            max_input_tokens: 100_000,
            max_output_tokens: 8_000,
            ..Default::default()
        };
        let low = resolve(&config, EffortLevel::Low, 0, false, 0);
        let limited = apply(config.clone(), low);
        assert_eq!(limited.max_rounds, 10);
        assert_eq!(limited.max_output_tokens, 2_000);
        assert_eq!(
            resolve(&config, EffortLevel::Auto, 0, false, 0).effective,
            EffortLevel::Medium
        );
        assert_eq!(
            resolve(&config, EffortLevel::Auto, 10, false, 0).effective,
            EffortLevel::High
        );
        assert_eq!(
            resolve(&config, EffortLevel::Auto, 15, false, 0).effective,
            EffortLevel::Xhigh
        );
        assert_eq!(
            resolve(&config, EffortLevel::High, 40, true, 0).effective,
            EffortLevel::High
        );
    }
}
