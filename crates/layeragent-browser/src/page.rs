//! Operations on one browser tab: navigate, read, interact, capture, observe.
//!
//! Everything returned to the model is page-controlled, so callers must treat it as
//! untrusted data. Dialogs, downloads and popups are neutralised rather than surfaced.
use crate::cdp::Cdp;
use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

const NETWORK_LOG: usize = 500;
const CONSOLE_LOG: usize = 200;
const TEXT_LIMIT: usize = 20_000;
const EVAL_LIMIT: usize = 20_000;
const MAX_SHOT_HEIGHT: u64 = 16_000;

const SNAPSHOT_JS: &str = r#"(() => {
  const LIMIT = 150, TEXT_LIMIT = 20000;
  const visible = el => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; };
  const sel = 'a[href],button,input,select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=tab],[onclick],[contenteditable=""],[contenteditable=true]';
  document.querySelectorAll('[data-la-id]').forEach(e => e.removeAttribute('data-la-id'));
  const all = document.querySelectorAll(sel); const out = []; let n = 0;
  for (const el of all) {
    if (out.length >= LIMIT) break;
    if (!visible(el)) continue;
    const id = ++n; el.setAttribute('data-la-id', String(id));
    const secret = el.type === 'password';
    const label = (el.getAttribute('aria-label') || el.innerText || el.placeholder || el.title || el.alt || '').trim().replace(/\s+/g, ' ').slice(0, 100);
    out.push({ id, tag: el.tagName.toLowerCase(), type: el.type || undefined, role: el.getAttribute('role') || undefined,
      text: label, href: el.href || undefined, name: el.name || undefined,
      value: secret ? undefined : (el.value ? String(el.value).slice(0, 100) : undefined),
      checked: typeof el.checked === 'boolean' ? el.checked : undefined, disabled: el.disabled || undefined });
  }
  return JSON.stringify({ url: location.href, title: document.title,
    text: (document.body ? document.body.innerText : '').slice(0, TEXT_LIMIT),
    elements: out, more_elements: all.length > LIMIT });
})()"#;

/// Bing: result cards are `li.b_algo`; the title link goes through a redirect whose `u`
/// parameter carries the real URL as "a1" + base64url.
const BING_JS: &str = r#"(() => {
  const out = [];
  document.querySelectorAll('li.b_algo').forEach(li => {
    const a = li.querySelector('h2 a'); if (!a) return;
    let href = a.href;
    try {
      const u = new URL(href);
      const enc = u.searchParams.get('u');
      if (u.hostname.endsWith('bing.com') && enc && enc.startsWith('a1')) {
        const b64 = enc.slice(2).replace(/-/g, '+').replace(/_/g, '/');
        href = atob(b64 + '='.repeat((4 - b64.length % 4) % 4));
      }
    } catch (e) {}
    const snip = li.querySelector('.b_caption p, p');
    const text = el => ((el.innerText || el.textContent) || '').replace(/\\s+/g, ' ').trim();
    out.push({ title: text(a), url: href, snippet: snip ? text(snip).slice(0, 300) : '' });
  });
  return JSON.stringify(out.slice(0, 10));
})()"#;

/// Any results page: external links that contain a heading, de-duplicated.
const GENERIC_JS: &str = r#"(() => {
  const seen = new Set(), out = [];
  document.querySelectorAll('a[href^="http"]').forEach(a => {
    const h = a.querySelector('h1,h2,h3,h4') || (a.parentElement && /^H[1-4]$/.test(a.parentElement.tagName) ? a.parentElement : null);
    if (!h || a.hostname === location.hostname || seen.has(a.href)) return;
    seen.add(a.href);
    const box = a.closest('article,li,div.result,.result') || a.parentElement;
    const text = (box ? (box.innerText || box.textContent) : '').replace(/\s+/g, ' ').trim();
    out.push({ title: ((h.innerText || h.textContent || a.textContent) || '').trim(), url: a.href, snippet: text.slice(0, 300) });
  });
  return JSON.stringify(out.slice(0, 10));
})()"#;

#[derive(Default)]
struct State {
    network: Mutex<VecDeque<Value>>,
    console: Mutex<VecDeque<Value>>,
    loading: AtomicBool,
    settled: Notify,
}

pub struct Page {
    cdp: Arc<Cdp>,
    target_id: String,
    session: String,
    state: Arc<State>,
    watcher: tokio::task::JoinHandle<()>,
    timeout: Duration,
}

fn push<T>(log: &Mutex<VecDeque<T>>, limit: usize, item: T) {
    if let Ok(mut log) = log.lock() {
        if log.len() >= limit {
            log.pop_front();
        }
        log.push_back(item);
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated]", &text[..end])
}

/// Only web pages are reachable by the agent; local files and browser internals are not.
pub fn check_url(url: &str) -> Result<()> {
    ensure!(
        !url.chars()
            .any(|c| c.is_ascii_control() || c.is_whitespace()),
        "URL cannot contain whitespace or control characters"
    );
    let parsed = reqwest::Url::parse(url).context("a full valid URL is required")?;
    ensure!(
        matches!(parsed.scheme(), "http" | "https"),
        "only http(s) URLs can be opened"
    );
    ensure!(parsed.host_str().is_some(), "URL has no host");
    ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "credentials in browser URLs are forbidden"
    );
    Ok(())
}

impl Page {
    /// Opens a new tab on `cdp` and starts observing it.
    pub async fn open(cdp: Arc<Cdp>, timeout: Duration) -> Result<Self> {
        let created = cdp
            .call("Target.createTarget", json!({"url":"about:blank"}), None)
            .await?;
        let target_id = created["targetId"]
            .as_str()
            .context("no target id")?
            .to_string();
        let attached = cdp
            .call(
                "Target.attachToTarget",
                json!({"targetId":target_id,"flatten":true}),
                None,
            )
            .await?;
        let session = attached["sessionId"]
            .as_str()
            .context("no session id")?
            .to_string();
        let state = Arc::new(State::default());
        let mut events = cdp.subscribe();
        for method in [
            "Page.enable",
            "Runtime.enable",
            "Network.enable",
            "Log.enable",
        ] {
            cdp.call(method, json!({}), Some(&session)).await?;
        }
        cdp.call("Target.setDiscoverTargets", json!({"discover":true}), None)
            .await?;
        // Nothing the page triggers may write to disk.
        let _ = cdp
            .call(
                "Browser.setDownloadBehavior",
                json!({"behavior":"deny"}),
                None,
            )
            .await;
        let watcher = {
            let (cdp, state, session, target) = (
                cdp.clone(),
                state.clone(),
                session.clone(),
                target_id.clone(),
            );
            tokio::spawn(async move {
                let mut popups = std::collections::HashSet::<String>::new();
                loop {
                    let event = match events.recv().await {
                        Ok(e) => e,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    };
                    let method = event["method"].as_str().unwrap_or("");
                    let params = &event["params"];
                    if event["sessionId"].as_str() == Some(session.as_str()) {
                        handle_page_event(&cdp, &state, &session, method, params).await;
                    } else if method.starts_with("Target.") {
                        handle_target_event(&cdp, &session, &target, &mut popups, method, params)
                            .await;
                    }
                }
            })
        };
        Ok(Self {
            cdp,
            target_id,
            session,
            state,
            watcher,
            timeout,
        })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.cdp.call(method, params, Some(&self.session)).await
    }

    async fn eval(&self, expression: &str) -> Result<Value> {
        let reply = self
            .call(
                "Runtime.evaluate",
                json!({"expression":expression,"returnByValue":true,"awaitPromise":true,"timeout":10000}),
            )
            .await?;
        if let Some(exception) = reply.get("exceptionDetails") {
            bail!(
                "page script failed: {}",
                exception["exception"]["description"]
                    .as_str()
                    .or(exception["text"].as_str())
                    .unwrap_or("error")
            );
        }
        Ok(reply["result"]["value"].clone())
    }

    /// Waits for the page to stop loading after something may have started a navigation.
    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let deadline = tokio::time::Instant::now() + self.timeout;
        while self.state.loading.load(Ordering::SeqCst) && tokio::time::Instant::now() < deadline {
            let _ = tokio::time::timeout(Duration::from_millis(250), self.state.settled.notified())
                .await;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    pub async fn navigate(&self, url: &str) -> Result<Value> {
        check_url(url)?;
        self.state.loading.store(true, Ordering::SeqCst);
        let reply = self.call("Page.navigate", json!({"url":url})).await?;
        if let Some(error) = reply["errorText"].as_str() {
            self.state.loading.store(false, Ordering::SeqCst);
            bail!("navigation failed: {error}");
        }
        self.settle().await;
        self.snapshot().await
    }

    /// Page text plus numbered interactive elements the other tools can act on.
    pub async fn snapshot(&self) -> Result<Value> {
        let raw = self.eval(SNAPSHOT_JS).await?;
        let mut snapshot: Value = serde_json::from_str(raw.as_str().context("empty snapshot")?)?;
        snapshot["text"] = json!(clip(snapshot["text"].as_str().unwrap_or(""), TEXT_LIMIT));
        snapshot["untrusted_web_content"] = json!(true);
        Ok(snapshot)
    }

    async fn locate(&self, id: u64) -> Result<(f64, f64)> {
        let reply = self
            .eval(&format!(
                "(()=>{{const el=document.querySelector('[data-la-id=\"{id}\"]');if(!el)return null;el.scrollIntoView({{block:'center',inline:'center'}});const r=el.getBoundingClientRect();return {{x:r.left+r.width/2,y:r.top+r.height/2}};}})()"
            ))
            .await?;
        let x = reply["x"]
            .as_f64()
            .context("element not found; take a new snapshot")?;
        Ok((x, reply["y"].as_f64().unwrap_or(0.0)))
    }

    pub async fn click(&self, id: u64) -> Result<Value> {
        let (x, y) = self.locate(id).await?;
        self.state.loading.store(false, Ordering::SeqCst);
        for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({"type":kind,"x":x,"y":y,"button":"left","buttons":buttons,"clickCount":1}),
            )
            .await?;
        }
        self.settle().await;
        self.snapshot().await
    }

    pub async fn type_text(&self, id: u64, text: &str, submit: bool) -> Result<Value> {
        self.locate(id).await?;
        self.eval(&format!(
            "(()=>{{const el=document.querySelector('[data-la-id=\"{id}\"]');el.focus();if(el.select)el.select();return true;}})()"
        ))
        .await?;
        // insertText replaces the selection, so existing field content is overwritten.
        self.call("Input.insertText", json!({"text":text})).await?;
        if submit {
            self.state.loading.store(false, Ordering::SeqCst);
            self.press_key("Enter").await?;
            self.settle().await;
        }
        self.snapshot().await
    }

    pub async fn press_key(&self, key: &str) -> Result<()> {
        let (code, vk, text) = match key {
            "Enter" => ("Enter", 13, "\r"),
            "Tab" => ("Tab", 9, ""),
            "Escape" => ("Escape", 27, ""),
            "Backspace" => ("Backspace", 8, ""),
            "ArrowDown" => ("ArrowDown", 40, ""),
            "ArrowUp" => ("ArrowUp", 38, ""),
            "ArrowLeft" => ("ArrowLeft", 37, ""),
            "ArrowRight" => ("ArrowRight", 39, ""),
            "PageDown" => ("PageDown", 34, ""),
            "PageUp" => ("PageUp", 33, ""),
            "Space" => ("Space", 32, " "),
            other => bail!("unsupported key {other}"),
        };
        let mut down = json!({"type":"keyDown","key":if key == "Space" { " " } else { key },"code":code,"windowsVirtualKeyCode":vk});
        if !text.is_empty() {
            down["text"] = json!(text);
        }
        self.call("Input.dispatchKeyEvent", down).await?;
        self.call(
            "Input.dispatchKeyEvent",
            json!({"type":"keyUp","key":key,"code":code,"windowsVirtualKeyCode":vk}),
        )
        .await?;
        Ok(())
    }

    pub async fn press(&self, key: &str) -> Result<Value> {
        self.state.loading.store(false, Ordering::SeqCst);
        self.press_key(key).await?;
        self.settle().await;
        self.snapshot().await
    }

    pub async fn scroll(&self, dy: i64) -> Result<Value> {
        self.eval(&format!("window.scrollBy(0,{})", dy.clamp(-20_000, 20_000)))
            .await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.snapshot().await
    }

    /// Waits until `text` appears on the page (or `timeout_ms` passes).
    pub async fn wait_for_text(&self, text: &str, timeout_ms: u64) -> Result<Value> {
        let needle = serde_json::to_string(text)?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms.min(120_000));
        loop {
            let found = self
                .eval(&format!(
                    "document.body&&document.body.innerText.includes({needle})"
                ))
                .await?;
            if found == json!(true) {
                return self.snapshot().await;
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "text did not appear in time"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>> {
        let mut params = json!({"format":"png"});
        if full_page {
            let metrics = self.call("Page.getLayoutMetrics", json!({})).await?;
            let size = &metrics["cssContentSize"];
            let width = size["width"].as_f64().unwrap_or(1280.0).clamp(1.0, 4096.0);
            let height = size["height"]
                .as_f64()
                .unwrap_or(900.0)
                .clamp(1.0, MAX_SHOT_HEIGHT as f64)
                .min(16_000_000.0 / width);
            params["captureBeyondViewport"] = json!(true);
            params["clip"] = json!({"x":0,"y":0,"width":width,"height":height,"scale":1});
        }
        let reply = self.call("Page.captureScreenshot", params).await?;
        let data = reply["data"].as_str().context("no screenshot data")?;
        Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
    }

    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        let value = self.eval(expression).await?;
        let text = serde_json::to_string(&value)?;
        Ok(json!({"result": clip(&text, EVAL_LIMIT), "untrusted_web_content": true}))
    }

    /// Reads a results page. `template` is a URL containing `{query}`; without one, Bing is used.
    pub async fn search(&self, query: &str, template: Option<&str>) -> Result<Value> {
        ensure!(
            !query.trim().is_empty() && query.len() <= 500,
            "query must be 1..500 bytes"
        );
        let mut encoder = reqwest::Url::parse("http://q.invalid/")?;
        encoder.query_pairs_mut().append_pair("q", query);
        let encoded = encoder
            .query()
            .unwrap_or("q=")
            .trim_start_matches("q=")
            .to_string();
        let (url, script) = match template {
            Some(template) => (template.replace("{query}", &encoded), GENERIC_JS),
            None => (
                format!("https://www.bing.com/search?q={encoded}&setlang=en"),
                BING_JS,
            ),
        };
        check_url(&url)?;
        self.state.loading.store(true, Ordering::SeqCst);
        self.call("Page.navigate", json!({"url":url})).await?;
        self.settle().await;
        // Result cards are filled in shortly after load on some engines.
        let mut results = json!([]);
        for _ in 0..16 {
            let raw = self.eval(script).await?;
            results = serde_json::from_str(raw.as_str().unwrap_or("[]"))?;
            if results.as_array().is_some_and(|r| !r.is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        ensure!(
            results.as_array().is_some_and(|r| !r.is_empty()),
            "no results were found on the search page (it may be showing a challenge); try browser.search_url or open a site directly"
        );
        Ok(json!({"query":query,"results":results,"untrusted_web_content":true}))
    }

    pub fn network(&self) -> Value {
        json!(
            self.state
                .network
                .lock()
                .map(|l| l.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        )
    }

    pub fn console(&self) -> Value {
        json!(
            self.state
                .console
                .lock()
                .map(|l| l.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        )
    }

    pub async fn close(&self) {
        let _ = self
            .cdp
            .call(
                "Target.closeTarget",
                json!({"targetId":self.target_id}),
                None,
            )
            .await;
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

async fn handle_page_event(cdp: &Cdp, state: &State, session: &str, method: &str, p: &Value) {
    match method {
        "Page.frameStartedLoading" => state.loading.store(true, Ordering::SeqCst),
        "Page.frameStoppedLoading" | "Page.loadEventFired" => {
            state.loading.store(false, Ordering::SeqCst);
            state.settled.notify_waiters();
        }
        "Page.javascriptDialogOpening" => {
            push(
                &state.console,
                CONSOLE_LOG,
                json!({"level":"dialog","text":clip(p["message"].as_str().unwrap_or(""), 500),"action":"dismissed"}),
            );
            let _ = cdp
                .call(
                    "Page.handleJavaScriptDialog",
                    json!({"accept":false}),
                    Some(session),
                )
                .await;
        }
        "Network.requestWillBeSent" => push(
            &state.network,
            NETWORK_LOG,
            json!({"id":p["requestId"],"method":p["request"]["method"],"url":clip(p["request"]["url"].as_str().unwrap_or(""), 300),"type":p["type"]}),
        ),
        "Network.responseReceived" => {
            if let Ok(mut log) = state.network.lock()
                && let Some(entry) = log.iter_mut().rev().find(|e| e["id"] == p["requestId"])
            {
                entry["status"] = p["response"]["status"].clone();
                entry["mime"] = p["response"]["mimeType"].clone();
            }
        }
        "Network.loadingFailed" => {
            if let Ok(mut log) = state.network.lock()
                && let Some(entry) = log.iter_mut().rev().find(|e| e["id"] == p["requestId"])
            {
                entry["failed"] = p["errorText"].clone();
                entry["blocked"] = p["blockedReason"].clone();
            }
        }
        "Runtime.consoleAPICalled" => {
            let text = p["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|v| {
                            v["value"].as_str().map_or_else(
                                || v["description"].as_str().unwrap_or("").to_string(),
                                str::to_string,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            push(
                &state.console,
                CONSOLE_LOG,
                json!({"level":p["type"],"text":clip(&text, 500)}),
            );
        }
        "Runtime.exceptionThrown" => push(
            &state.console,
            CONSOLE_LOG,
            json!({"level":"exception","text":clip(p["exceptionDetails"]["exception"]["description"].as_str().or(p["exceptionDetails"]["text"].as_str()).unwrap_or(""), 500)}),
        ),
        "Log.entryAdded" => push(
            &state.console,
            CONSOLE_LOG,
            json!({"level":p["entry"]["level"],"text":clip(p["entry"]["text"].as_str().unwrap_or(""), 500),"url":p["entry"]["url"]}),
        ),
        _ => {}
    }
}

/// Popups are closed and their URL loaded in the main tab, so links that open new tabs work
/// without the agent ever having to track more than one page.
async fn handle_target_event(
    cdp: &Cdp,
    session: &str,
    main_target: &str,
    popups: &mut std::collections::HashSet<String>,
    method: &str,
    p: &Value,
) {
    if !matches!(method, "Target.targetCreated" | "Target.targetInfoChanged") {
        return;
    }
    let info = &p["targetInfo"];
    let id = info["targetId"].as_str().unwrap_or("");
    if info["type"] != "page" || id == main_target {
        return;
    }
    if method == "Target.targetCreated" && info["openerId"].as_str() == Some(main_target) {
        popups.insert(id.to_string());
    }
    let url = info["url"].as_str().unwrap_or("");
    if popups.contains(id) && check_url(url).is_ok() {
        popups.remove(id);
        let _ = cdp
            .call("Target.closeTarget", json!({"targetId":id}), None)
            .await;
        let _ = cdp
            .call("Page.navigate", json!({"url":url}), Some(session))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_urls_with_a_host_are_navigable() {
        for ok in [
            "https://example.com",
            "http://example.com:8080/a?b#c",
            "HTTPS://Example.com",
        ] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "chrome://settings",
            "data:text/html,hi",
            "about:blank",
            "https://",
            "example.com",
            "view-source:https://example.com",
            "https://user:secret@example.com",
            "https://example.com\n",
            "https://example.com:invalid",
            "https://[::1",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn clip_respects_char_boundaries() {
        assert_eq!(clip("áéí", 3), "á…[truncated]");
        assert_eq!(clip("short", 10), "short");
    }
}
