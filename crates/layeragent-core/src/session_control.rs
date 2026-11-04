//! Engine-owned session operations. They execute under the current turn's lease.
use super::*;
use layeragent_tools::SessionControl;

pub(crate) struct EngineSessionControl {
    pub engine: Arc<Engine>,
    pub selected: String,
    /// What the current turn has spent when the tool runs.
    pub spend: TurnSpend,
    pub ctl: Arc<agents::AgentCtl>,
    pub team: Option<Arc<cooperative::TeamRuntime>>,
}

/// The current turn's consumption so far.
#[derive(Clone, Default)]
pub(crate) struct TurnSpend {
    pub usage: Usage,
    pub cost_usd: f64,
    pub rounds: usize,
}

/// Models a turn may hand work to, cheapest input first: configured, allowed
/// by the privacy settings and by their team access.
fn delegable_models(config: &Config, selected: &str) -> Vec<Value> {
    let mut models: Vec<(&String, &layeragent_config::Model)> = config
        .models
        .iter()
        .filter(|(alias, model)| {
            config.require_model_egress(alias).is_ok()
                && model.team_access != layeragent_config::ModelTeamAccess::None
                && (model.tools || model.emulate_tools)
        })
        .collect();
    models.sort_by(|a, b| {
        let price = |m: &layeragent_config::Model| m.input_per_million.unwrap_or(f64::INFINITY);
        price(a.1).total_cmp(&price(b.1)).then_with(|| a.0.cmp(b.0))
    });
    models
        .into_iter()
        .map(|(alias, model)| {
            json!({
                "alias": alias,
                "current": alias == selected,
                "input_per_million": model.input_per_million,
                "output_per_million": model.output_per_million,
                "cache_read_per_million": model.cache_read_per_million,
                "prompt_cache": model.prompt_cache,
                "context_window": model.context_window,
                "may_write": model.team_access == layeragent_config::ModelTeamAccess::Write,
                "notes": model.notes,
            })
        })
        .collect()
}

impl EngineSessionControl {
    async fn budget_status(&self, ctx: &ToolContext<'_>) -> Result<Value> {
        let usage = &self.spend.usage;
        let ratio = |read: u64, input: u64| {
            if input == 0 {
                0.0
            } else {
                read as f64 / input as f64
            }
        };
        let budget = &self.ctl.tree.budget;
        let mut status = json!({
            "turn": {
                "model": self.selected,
                "rounds_completed": self.spend.rounds,
                "max_rounds": ctx.config.max_rounds,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cache_read_tokens": usage.cache_read_tokens,
                "cache_write_tokens": usage.cache_write_tokens,
                "cache_read_ratio": ratio(usage.cache_read_tokens, usage.input_tokens),
                "cost_usd": self.spend.cost_usd,
                "max_cost_usd": ctx.config.max_cost_usd,
            },
            "task": {
                "child_agent": self.ctl.depth > 0,
                "spent_usd": budget.spent_usd(),
                "budget_usd": budget.cost_limit(),
                "remaining_usd": budget.remaining_usd(),
                "spent_tokens": budget.spent_tokens(),
                "token_limit": budget.token_limit(),
                "budget_share_used": budget.share(),
            },
            "models": delegable_models(ctx.config, &self.selected),
            "estimate": "costs are estimates from the configured prices, not provider invoices",
        });
        if let Some(team) = &self.team {
            let snapshot = ctx.store.team_snapshot_by_id(&team.context.team_id).await?;
            status["team"] = json!({
                "usage": snapshot.usage,
                "reservations": snapshot.reservations,
                "limits": snapshot.options.limits,
                "usage_by_model": snapshot.usage_by_model,
            });
        }
        Ok(status)
    }
}

#[async_trait::async_trait]
impl SessionControl for EngineSessionControl {
    async fn execute(&self, ctx: &ToolContext<'_>, name: &str, _args: &Value) -> Result<Value> {
        match name {
            "budget_status" => self.budget_status(ctx).await,
            "context_status" => {
                self.engine
                    .context_status_for(ctx.scope, ctx.session_id, &self.selected)
                    .await
            }
            "compact_context" => {
                let rows = ctx.store.history_since(ctx.session_id, 0).await?;
                let through = context::completed_request_boundary(&rows)
                    .context("no completed history before the current request")?;
                // No tool from this request, including this call, is covered.
                ensure!(through > 0, "no safe checkpoint boundary");
                let (checkpoint, _, _) = self
                    .engine
                    .compact_context(
                        ctx.scope,
                        ctx.store,
                        ctx.session_id,
                        Some(ctx.turn_id),
                        &self.selected,
                        Arc::new(ctx.config.clone()),
                        false,
                        ctx.cancel.clone(),
                    )
                    .await?;
                Ok(checkpoint)
            }
            _ => bail!("unknown session operation"),
        }
    }
}
