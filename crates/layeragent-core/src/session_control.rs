//! Engine-owned session operations. They execute under the current turn's lease.
use super::*;
use layeragent_tools::SessionControl;

pub(crate) struct EngineSessionControl {
    pub engine: Arc<Engine>,
    pub selected: String,
}

#[async_trait::async_trait]
impl SessionControl for EngineSessionControl {
    async fn execute(&self, ctx: &ToolContext<'_>, name: &str, _args: &Value) -> Result<Value> {
        match name {
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
