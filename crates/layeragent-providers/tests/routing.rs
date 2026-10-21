use anyhow::Result;
use layeragent_config::{Config, Model, Provider, Wire};
use layeragent_protocol::Message;
use layeragent_providers::{LanguageModel, Router};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn transient_failure_uses_only_authorized_fallback() -> Result<()> {
    let app=axum::Router::new().route("/unavailable/chat/completions",axum::routing::post(||async{axum::http::StatusCode::TOO_MANY_REQUESTS})).route("/healthy/chat/completions",axum::routing::post(||async{([("content-type","text/event-stream")],"data: {\"choices\":[{\"delta\":{\"content\":\"verified\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut config = Config::default();
    for (name, path) in [("first", "unavailable"), ("second", "healthy")] {
        config.providers.insert(
            name.into(),
            Provider {
                wire: Wire::OpenaiChat,
                base_url: format!("{base}/{path}"),
                key_env: None,
                oauth: None,
                headers: BTreeMap::new(),
            },
        );
        config.models.insert(
            name.into(),
            Model {
                provider: name.into(),
                id: name.into(),
                context_window: 8192,
                tools: true,
                emulate_tools: false,
                input_per_million: None,
                output_per_million: None,
                parameters: Default::default(),
            },
        );
    }
    config.primary = "first".into();
    config.fallbacks.push("second".into());
    config.validate()?;
    let router = Router::new()?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let result = router
        .infer(
            &config,
            "first",
            &[Message::text("user", "hello")],
            &[],
            CancellationToken::new(),
            tx,
        )
        .await?;
    assert_eq!(result.text, "verified");
    assert_eq!(result.model, "second");
    let mut discarded = false;
    while let Ok(event) = rx.try_recv() {
        discarded |= event.kind == "attempt_discarded";
    }
    assert!(discarded);
    task.abort();
    Ok(())
}
