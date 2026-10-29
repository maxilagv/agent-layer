use layeragent_config::Config;
use layeragent_protocol::EffortLevel;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EffortPolicy {
    pub(crate) requested: EffortLevel,
    pub(crate) effective: EffortLevel,
}

pub(crate) fn resolve(
    config: &Config,
    requested: EffortLevel,
    completed_rounds: usize,
    context_pressure: bool,
) -> EffortPolicy {
    let effective = if requested == EffortLevel::Auto {
        let pressure = completed_rounds.saturating_mul(8) >= config.max_rounds.saturating_mul(3);
        let elevated = completed_rounds.saturating_mul(4) >= config.max_rounds || context_pressure;
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
    fn ceilings_and_bounded_auto_escalation_hold() {
        let config = Config {
            max_rounds: 40,
            max_input_tokens: 100_000,
            max_output_tokens: 8_000,
            ..Default::default()
        };
        let low = resolve(&config, EffortLevel::Low, 0, false);
        let limited = apply(config.clone(), low);
        assert_eq!(limited.max_rounds, 10);
        assert_eq!(limited.max_output_tokens, 2_000);
        assert_eq!(
            resolve(&config, EffortLevel::Auto, 0, false).effective,
            EffortLevel::Medium
        );
        assert_eq!(
            resolve(&config, EffortLevel::Auto, 10, false).effective,
            EffortLevel::High
        );
        assert_eq!(
            resolve(&config, EffortLevel::Auto, 15, false).effective,
            EffortLevel::Xhigh
        );
        assert_eq!(
            resolve(&config, EffortLevel::High, 40, true).effective,
            EffortLevel::High
        );
    }
}
