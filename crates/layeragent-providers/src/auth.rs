use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use layeragent_config::Provider;
use layeragent_protocol::{new_id, now_ms};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Serialize, Deserialize)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: i64,
}
fn credential_identity(name: &str, provider: &Provider) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&(
        name,
        &provider.wire,
        &provider.base_url,
        &provider.oauth,
    ))?)))
}
fn entry(name: &str, provider: &Provider) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(
        "layeragent",
        &credential_identity(name, provider)?,
    )?)
}

/// The credential store entry that holds a plain API key.
fn api_key_entry(name: &str, provider: &Provider) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(
        "layeragent",
        &format!("{}:api-key", credential_identity(name, provider)?),
    )?)
}

/// Stores an API key in the operating system credential store.
///
/// The key never enters the configuration file or any assistant state; only
/// the store holds it.
pub fn store_api_key(name: &str, provider: &Provider, value: &str) -> Result<()> {
    ensure!(!value.trim().is_empty(), "the credential is empty");
    ensure!(
        !value.contains('\n') && !value.contains('\r'),
        "the credential must be a single line"
    );
    api_key_entry(name, provider)?
        .set_password(value.trim())
        .context("the system credential store rejected the credential")?;
    layeragent_protocol::redact::register_secret(value.trim());
    Ok(())
}

/// Removes a stored API key.
pub fn forget_api_key(name: &str, provider: &Provider) -> Result<()> {
    match api_key_entry(name, provider)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("No matching entry") => Ok(()),
        Err(error) => Err(error.into()),
    }
}
pub async fn credential(
    client: &reqwest::Client,
    name: &str,
    p: &Provider,
) -> Result<Option<String>> {
    if let Some(key) = &p.key_env {
        let value = std::env::var(key)
            .with_context(|| format!("missing credential environment variable {key}"))?;
        layeragent_protocol::redact::register_secret(&value);
        return Ok(Some(value));
    }
    // A key stored by the guided setup is read from the system credential
    // store. The environment variable still wins when both exist.
    if let Ok(value) = api_key_entry(name, p)?.get_password()
        && !value.is_empty()
    {
        layeragent_protocol::redact::register_secret(&value);
        return Ok(Some(value));
    }
    let Some(oauth) = &p.oauth else {
        return Ok(None);
    };
    let raw = entry(name, p)?
        .get_password()
        .context("OAuth login required")?;
    let mut token: Token = serde_json::from_str(&raw)?;
    register_token(&token);
    if token.expires_at < now_ms() + 60_000 {
        let refresh = token
            .refresh_token
            .as_deref()
            .context("OAuth login expired; login again")?;
        let response = client
            .post(&oauth.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", oauth.client_id.as_str()),
                ("refresh_token", refresh),
            ])
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "OAuth refresh rejected; login again"
        );
        let v: Value = response.json().await?;
        token = token_from(v, token.refresh_token)?;
        register_token(&token);
        entry(name, p)?.set_password(&serde_json::to_string(&token)?)?;
    }
    Ok(Some(token.access_token))
}
fn register_token(token: &Token) {
    layeragent_protocol::redact::register_secret(&token.access_token);
    if let Some(refresh) = &token.refresh_token {
        layeragent_protocol::redact::register_secret(refresh);
    }
}
fn token_from(v: Value, previous: Option<String>) -> Result<Token> {
    Ok(Token {
        access_token: v["access_token"]
            .as_str()
            .context("OAuth response missing access_token")?
            .into(),
        refresh_token: v["refresh_token"].as_str().map(str::to_string).or(previous),
        expires_at: now_ms() + v["expires_in"].as_i64().unwrap_or(3600) * 1000,
    })
}
pub fn logout(name: &str, p: &Provider) -> Result<()> {
    entry(name, p)?.delete_credential()?;
    Ok(())
}
pub async fn login(name: &str, p: &Provider) -> Result<()> {
    let o = p
        .oauth
        .as_ref()
        .context("provider has no official OAuth configuration")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let token = if let Some(device) = &o.device_url {
        ensure!(
            device.starts_with("https://"),
            "device endpoint must use HTTPS"
        );
        let response = client
            .post(device)
            .form(&[
                ("client_id", o.client_id.as_str()),
                ("scope", &o.scopes.join(" ")),
            ])
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "device authorization rejected"
        );
        let v: Value = response.json().await?;
        eprintln!(
            "Open {} and enter {}",
            v["verification_uri"]
                .as_str()
                .context("missing verification URI")?,
            v["user_code"].as_str().context("missing user code")?
        );
        let code = v["device_code"].as_str().context("missing device code")?;
        let mut interval = v["interval"].as_u64().unwrap_or(5).max(1);
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(v["expires_in"].as_u64().unwrap_or(600).min(900));
        loop {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "device login expired"
            );
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let response = client
                .post(&o.token_url)
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", o.client_id.as_str()),
                    ("device_code", code),
                ])
                .send()
                .await?;
            let v: Value = response.json().await?;
            match v["error"].as_str() {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    interval += 5;
                    continue;
                }
                Some(_) => bail!("OAuth device login rejected"),
                None => break token_from(v, None)?,
            }
        }
    } else {
        let auth = o
            .authorization_url
            .as_ref()
            .context("authorization_url or device_url required")?;
        ensure!(
            auth.starts_with("https://"),
            "authorization endpoint must use HTTPS"
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let redirect = format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr()?.port()
        );
        let state = new_id();
        let verifier = format!("{}{}", new_id(), new_id());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let mut url = reqwest::Url::parse(auth)?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", &o.client_id),
            ("redirect_uri", &redirect),
            ("scope", &o.scopes.join(" ")),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        eprintln!("Open this URL to authorize LayerAgent:\n{url}");
        let code=tokio::time::timeout(Duration::from_secs(180),async{loop{let(mut socket,_)=listener.accept().await?;let mut raw=vec![0;8192];let n=tokio::time::timeout(Duration::from_secs(5),socket.read(&mut raw)).await??;let request=String::from_utf8_lossy(&raw[..n]);let path=request.lines().next().and_then(|l|l.split_whitespace().nth(1)).unwrap_or("");let parsed=reqwest::Url::parse(&format!("http://localhost{path}"))?;let args:BTreeMap<_,_>=parsed.query_pairs().map(|(k,v)|(k.into_owned(),v.into_owned())).collect();let valid=parsed.path()=="/callback" && args.get("state")==Some(&state) && args.contains_key("code");socket.write_all(if valid{b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\nAuthorization saved."}else{b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"}).await?;if valid{return Ok::<_,anyhow::Error>(args["code"].clone());}}}).await??;
        let response = client
            .post(&o.token_url)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", o.client_id.as_str()),
                ("code", &code),
                ("redirect_uri", &redirect),
                ("code_verifier", &verifier),
            ])
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "OAuth code exchange rejected"
        );
        token_from(response.json().await?, None)?
    };
    register_token(&token);
    entry(name, p)?
        .set_password(&serde_json::to_string(&token)?)
        .context("secure credential store unavailable; tokens were not written to plaintext")?;
    eprintln!("OAuth credentials saved in the OS keyring.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_are_bound_to_provider_endpoint_and_client() -> Result<()> {
        let mut provider = Provider {
            wire: layeragent_config::Wire::OpenaiChat,
            base_url: "https://api.example.test/v1".into(),
            key_env: None,
            oauth: None,
            headers: Default::default(),
        };
        let identity = credential_identity("primary", &provider)?;
        provider.base_url = "https://other.example.test/v1".into();
        assert_ne!(identity, credential_identity("primary", &provider)?);
        assert_ne!(
            credential_identity("other", &provider)?,
            credential_identity("primary", &provider)?
        );
        Ok(())
    }
}
