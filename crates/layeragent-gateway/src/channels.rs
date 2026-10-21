use super::*;
use futures_util::{SinkExt, StreamExt};
use layeragent_config::Channel;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub fn start(engine: Arc<Engine>) {
    tokio::spawn(async move {
        let mut running: HashMap<String, (String, CancellationToken)> = HashMap::new();
        loop {
            if engine.shutdown.is_cancelled() {
                for (_, (_, token)) in running {
                    token.cancel();
                }
                break;
            }
            let config = engine.config().await;
            let wanted = config
                .channels
                .iter()
                .filter(|(_, c)| c.mode == "native")
                .map(|(n, c)| (n.clone(), serde_json::to_string(c).unwrap_or_default()))
                .collect::<HashMap<_, _>>();
            running.retain(|name, (revision, token)| {
                if wanted.get(name) != Some(revision) {
                    token.cancel();
                    false
                } else {
                    true
                }
            });
            for (name, revision) in wanted {
                if running.contains_key(&name) {
                    continue;
                }
                let cancel = engine.shutdown.child_token();
                running.insert(name.clone(), (revision, cancel.clone()));
                let e = engine.clone();
                tokio::spawn(async move {
                    let mut backoff = 1;
                    let mut discord = DiscordSession::default();
                    loop {
                        let cfg = e.config().await;
                        let Some(channel) = cfg.channels.get(&name) else {
                            break;
                        };
                        let future = async {
                            match channel.kind.as_str() {
                                "telegram" => telegram(&e, &name, channel, &cancel).await,
                                "matrix" => matrix(&e, &name, channel, &cancel).await,
                                "signal" => signal(&e, &name, channel, &cancel).await,
                                "discord" => {
                                    discord_gateway(&e, &name, channel, &mut discord, &cancel).await
                                }
                                _ => anyhow::bail!("this platform uses signed webhooks"),
                            }
                        };
                        tokio::select! {_=cancel.cancelled()=>break,result=future=>if let Err(error)=result{tracing::warn!(channel=%name,error=%error,"connector disconnected");}}
                        tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_secs(backoff))=>{}}
                        backoff = (backoff * 2).min(60);
                    }
                });
            }
            tokio::select! {_=engine.shutdown.cancelled()=>{},_=tokio::time::sleep(std::time::Duration::from_secs(2))=>{}}
        }
    });
}

pub async fn enqueue(engine: &Engine, name: &str, event: PlatformEvent) -> Result<bool> {
    if event.is_bot || event.text.trim().is_empty() {
        return Ok(false);
    }
    let config = engine.config().await;
    let Some(access) = config.access.iter().find(|a| {
        a.platform.as_deref() == Some(name)
            && a.sender_id.as_deref() == Some(&event.sender_id)
            && a.channel_id
                .as_ref()
                .is_none_or(|id| id == &event.channel_id)
            && !a.read_only
    }) else {
        return Ok(false);
    };
    let scope = Scope {
        tenant: access.tenant.clone(),
        principal: access.principal.clone(),
    };
    let store = engine.store(&scope).await?;
    store
        .enqueue_inbox(
            name,
            &event.external_id,
            &scope.principal,
            &event.channel_id,
            &event.text,
        )
        .await
}
async fn cursor(engine: &Engine, name: &str) -> Result<Option<String>> {
    let store = engine.store(&Scope::local()).await?;
    let n = name.to_string();
    store
        .read(move |db| {
            let result = db.query_row(
                "SELECT cursor FROM connector_cursors WHERE name=?1",
                [n],
                |r| r.get::<_, String>(0),
            );
            Ok(result.ok())
        })
        .await
}
async fn save_cursor(engine: &Engine, name: &str, value: String) -> Result<()> {
    let store = engine.store(&Scope::local()).await?;
    let n = name.to_string();
    store.write(move|db|{db.execute("INSERT INTO connector_cursors(name,cursor) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET cursor=excluded.cursor",[n,value])?;Ok(())}).await
}
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
async fn telegram(
    engine: &Engine,
    name: &str,
    channel: &Channel,
    cancel: &CancellationToken,
) -> Result<()> {
    let token = std::env::var(&channel.token_env).context("Telegram token unavailable")?;
    let client = client()?;
    let mut offset = cursor(engine, name)
        .await?
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    loop {
        let response = tokio::select! {_=cancel.cancelled()=>return Ok(()),r=client.post(format!("https://api.telegram.org/bot{token}/getUpdates")).json(&json!({"offset":offset,"timeout":25,"allowed_updates":["message"]})).send()=>r.map_err(|_|anyhow::anyhow!("Telegram polling failed"))?};
        anyhow::ensure!(
            response.status().is_success(),
            "Telegram HTTP {}",
            response.status().as_u16()
        );
        let body: Value = response.json().await?;
        anyhow::ensure!(body["ok"] == true, "Telegram rejected getUpdates");
        for update in body["result"].as_array().into_iter().flatten() {
            for event in normalize("telegram", update)? {
                enqueue(engine, name, event).await?;
            }
            offset = offset.max(
                update["update_id"]
                    .as_i64()
                    .context("invalid Telegram update id")?
                    + 1,
            );
            save_cursor(engine, name, offset.to_string()).await?;
        }
    }
}
async fn matrix(
    engine: &Engine,
    name: &str,
    channel: &Channel,
    cancel: &CancellationToken,
) -> Result<()> {
    let token = std::env::var(&channel.token_env).context("Matrix token unavailable")?;
    let base = channel
        .endpoint
        .as_ref()
        .context("Matrix homeserver endpoint required")?
        .trim_end_matches('/');
    let client = client()?;
    let me: Value = client
        .get(format!("{base}/_matrix/client/v3/account/whoami"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let own = me["user_id"].as_str().context("Matrix identity missing")?;
    let mut since = cursor(engine, name).await?;
    loop {
        let mut request = client
            .get(format!("{base}/_matrix/client/v3/sync"))
            .bearer_auth(&token)
            .query(&[("timeout", "25000")]);
        if let Some(since) = &since {
            request = request.query(&[("since", since)]);
        }
        let response = tokio::select! {_=cancel.cancelled()=>return Ok(()),r=request.send()=>r?};
        let body: Value = response.error_for_status()?.json().await?;
        if since.is_some() {
            for (room, data) in body["rooms"]["join"].as_object().into_iter().flatten() {
                for event in data["timeline"]["events"].as_array().into_iter().flatten() {
                    if event["type"] != "m.room.message" {
                        continue;
                    }
                    let sender = event["sender"].as_str().unwrap_or("");
                    enqueue(
                        engine,
                        name,
                        PlatformEvent {
                            external_id: event["event_id"]
                                .as_str()
                                .context("Matrix event ID required")?
                                .into(),
                            platform: "matrix".into(),
                            channel_id: room.clone(),
                            sender_id: sender.into(),
                            is_bot: sender == own,
                            text: event["content"]["body"].as_str().unwrap_or("").into(),
                            attachments: vec![],
                        },
                    )
                    .await?;
                }
            }
        }
        since = Some(
            body["next_batch"]
                .as_str()
                .context("Matrix sync cursor missing")?
                .into(),
        );
        save_cursor(engine, name, since.clone().unwrap_or_default()).await?;
    }
}
#[cfg(unix)]
async fn signal(
    engine: &Engine,
    name: &str,
    channel: &Channel,
    cancel: &CancellationToken,
) -> Result<()> {
    let path = channel
        .endpoint
        .as_deref()
        .and_then(|s| s.strip_prefix("unix:"))
        .context("Signal requires endpoint = unix:/path/to/socket")?;
    let socket = tokio::net::UnixStream::connect(path).await?;
    let (read, mut write) = socket.into_split();
    let mut params = json!({});
    if let Some(account) = &channel.account {
        params["account"] = json!(account);
    }
    write.write_all(format!("{}\n",json!({"jsonrpc":"2.0","method":"subscribeReceive","id":"layeragent","params":params})).as_bytes()).await?;
    let mut lines = BufReader::new(read).lines();
    loop {
        let line = tokio::select! {_=cancel.cancelled()=>return Ok(()),r=lines.next_line()=>r?.context("Signal socket closed")?};
        anyhow::ensure!(line.len() <= 1_048_576, "Signal frame too large");
        let value: Value = serde_json::from_str(&line)?;
        if value["method"] != "receive" {
            continue;
        }
        let envelope = value["params"]
            .get("result")
            .unwrap_or(&value["params"])
            .get("envelope")
            .context("Signal envelope missing")?;
        let sender = envelope["sourceNumber"]
            .as_str()
            .or_else(|| envelope["sourceUuid"].as_str())
            .unwrap_or("");
        let message = &envelope["dataMessage"];
        let destination = message["groupInfo"]["groupId"]
            .as_str()
            .map(|g| format!("group:{g}"))
            .unwrap_or_else(|| sender.to_string());
        let text = message["message"].as_str().unwrap_or("");
        enqueue(
            engine,
            name,
            PlatformEvent {
                external_id: format!("{}:{sender}:{destination}", envelope["timestamp"]),
                platform: "signal".into(),
                channel_id: destination,
                sender_id: sender.into(),
                is_bot: false,
                text: text.into(),
                attachments: vec![],
            },
        )
        .await?;
    }
}
#[cfg(not(unix))]
async fn signal(_: &Engine, _: &str, _: &Channel, _: &CancellationToken) -> Result<()> {
    anyhow::bail!("Signal UNIX sockets are unavailable on this platform")
}
#[derive(Default)]
struct DiscordSession {
    session: Option<String>,
    url: Option<String>,
    seq: Option<u64>,
}
async fn discord_gateway(
    engine: &Engine,
    name: &str,
    channel: &Channel,
    session: &mut DiscordSession,
    cancel: &CancellationToken,
) -> Result<()> {
    use tokio_tungstenite::tungstenite::Message;
    let token = std::env::var(&channel.token_env).context("Discord token unavailable")?;
    let base = if let Some(url) = &session.url {
        url.clone()
    } else {
        let response: Value = client()?
            .get("https://discord.com/api/v10/gateway/bot")
            .header("authorization", format!("Bot {token}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        response["url"]
            .as_str()
            .context("Discord gateway URL missing")?
            .into()
    };
    anyhow::ensure!(base.starts_with("wss://"), "Discord gateway must use TLS");
    let (mut socket, _) =
        tokio_tungstenite::connect_async(format!("{base}/?v=10&encoding=json")).await?;
    let hello = socket
        .next()
        .await
        .context("Discord disconnected before hello")??;
    let hello: Value = serde_json::from_str(hello.to_text()?)?;
    let interval = hello["d"]["heartbeat_interval"]
        .as_u64()
        .context("Discord heartbeat interval missing")?;
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_millis(interval.max(1000)));
    let mut acknowledged = true;
    let identify = if let Some(id) = &session.session {
        json!({"op":6,"d":{"token":token,"session_id":id,"seq":session.seq}})
    } else {
        json!({"op":2,"d":{"token":token,"intents":channel.intents.unwrap_or(37377),"properties":{"os":std::env::consts::OS,"browser":"LayerAgent","device":"LayerAgent"}}})
    };
    socket
        .send(Message::Text(identify.to_string().into()))
        .await?;
    loop {
        tokio::select! {_=cancel.cancelled()=>{let _=socket.close(None).await;return Ok(())},_=heartbeat.tick()=>{anyhow::ensure!(acknowledged,"Discord heartbeat was not acknowledged");acknowledged=false;socket.send(Message::Text(json!({"op":1,"d":session.seq}).to_string().into())).await?;},message=socket.next()=>{let message=message.context("Discord gateway closed")??;if let Message::Close(frame)=&message{if frame.as_ref().is_some_and(|f|[4007,4009].contains(&u16::from(f.code))){session.session=None;}anyhow::bail!("Discord gateway closed");}let Ok(text)=message.to_text()else{continue};anyhow::ensure!(text.len()<=1_048_576,"Discord frame too large");let event:Value=serde_json::from_str(text)?;if let Some(seq)=event["s"].as_u64(){session.seq=Some(seq);}match event["op"].as_u64(){Some(11)=>acknowledged=true,Some(1)=>socket.send(Message::Text(json!({"op":1,"d":session.seq}).to_string().into())).await?,Some(7)=>return Ok(()),Some(9)=>{session.session=None;anyhow::bail!("Discord session invalidated");},Some(0)=>match event["t"].as_str(){Some("READY")=>{session.session=event["d"]["session_id"].as_str().map(str::to_string);session.url=event["d"]["resume_gateway_url"].as_str().map(str::to_string);},Some("MESSAGE_CREATE")=>{for message in normalize("discord",&event["d"])?{enqueue(engine,name,message).await?;}},_=>{}},_=>{}}}}
    }
}
pub async fn signal_send(channel: &Channel, destination: &str, text: &str) -> Result<()> {
    #[cfg(unix)]
    {
        let path = channel
            .endpoint
            .as_deref()
            .and_then(|s| s.strip_prefix("unix:"))
            .context("Signal UNIX endpoint required")?;
        let socket = tokio::net::UnixStream::connect(path).await?;
        let (read, mut write) = socket.into_split();
        let mut params = json!({"message":text});
        if let Some(group) = destination.strip_prefix("group:") {
            params["groupId"] = json!(group);
        } else {
            params["recipient"] = json!([destination]);
        }
        if let Some(account) = &channel.account {
            params["account"] = json!(account);
        }
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0","method":"send","params":params,"id":"layeragent-send"})
                )
                .as_bytes(),
            )
            .await?;
        let mut lines = BufReader::new(read).lines();
        loop {
            let line = lines.next_line().await?.context("Signal socket closed")?;
            let v: Value = serde_json::from_str(&line)?;
            if v["id"] == "layeragent-send" {
                anyhow::ensure!(v.get("error").is_none(), "Signal rejected send");
                return Ok(());
            }
        }
    }
    #[cfg(not(unix))]
    anyhow::bail!("Signal sockets are unavailable")
}
