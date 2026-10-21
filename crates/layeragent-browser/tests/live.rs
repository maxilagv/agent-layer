#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Drives a real Chromium against a local fixture site. Needs network once (to download
//! the managed browser) and permission to run Chromium, so it is ignored by default:
//! `cargo test -p layeragent-browser --test live -- --ignored --nocapture`.
use layeragent_browser::BrowserManager;
use layeragent_config::BrowserConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const INDEX: &str = r#"<!doctype html><html><head><title>Fixture Home</title></head><body>
<h1>Welcome to the fixture</h1>
<a href="/next">Go to next page</a>
<a href="/next" target="_blank">Open in new tab</a>
<form action="/result" method="get">
  <input name="q" placeholder="Search box" id="q">
  <input type="password" name="pw" value="hunter2hunter2">
  <button type="submit">Send it</button>
</form>
<script>console.log("fixture loaded"); fetch("/api/ping").then(r => r.text());
window.alert("hi there");</script></body></html>"#;

async fn serve(port_tx: tokio::sync::oneshot::Sender<u16>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    port_tx.send(listener.local_addr().unwrap().port()).unwrap();
    loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let mut buffer = vec![0u8; 8192];
            let n = socket.read(&mut buffer).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, body) = match path.split('?').next().unwrap_or("/") {
                "/" => (200, INDEX.to_string()),
                "/next" => (200, "<title>Second</title><h1>Second page</h1>".to_string()),
                "/result" => (
                    200,
                    format!("<title>Result</title><p>You searched: {}</p>", path),
                ),
                "/api/ping" => (200, "pong".to_string()),
                _ => (404, "not found".to_string()),
            };
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        });
    }
}

fn config() -> BrowserConfig {
    BrowserConfig {
        allow_domains: vec!["127.0.0.1".into()],
        ..BrowserConfig::default()
    }
}

#[tokio::test]
#[ignore = "downloads and runs a real Chromium"]
async fn reads_navigates_interacts_and_observes_a_real_page() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(serve(tx));
    let port = rx.await.unwrap();
    let home = std::path::PathBuf::from(
        std::env::var("LAYERAGENT_BROWSER_TEST_HOME")
            .unwrap_or_else(|_| "/tmp/layeragent-browser-test-home".into()),
    );
    let manager = BrowserManager::for_tests_allowing_private_network();
    let session = manager.open("owner", &home, &config(), None).await.unwrap();

    let page = &session.page;
    let home_view = page
        .navigate(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    assert_eq!(home_view["title"], "Fixture Home");
    assert!(
        session.proxy_decisions().iter().any(|decision| {
            decision.allowed && decision.host == "127.0.0.1" && decision.port == port
        }),
        "successful navigation must traverse the confined, policy-enforcing proxy"
    );
    assert!(
        home_view["text"]
            .as_str()
            .unwrap()
            .contains("Welcome to the fixture")
    );
    assert_eq!(home_view["untrusted_web_content"], true);
    let elements = home_view["elements"].as_array().unwrap();
    let find = |needle: &str| {
        elements
            .iter()
            .find(|e| e["text"].as_str().is_some_and(|t| t.contains(needle)))
            .unwrap()["id"]
            .as_u64()
            .unwrap()
    };
    // Password values are never echoed.
    assert!(!home_view.to_string().contains("hunter2hunter2"));

    // Type into the form and submit it with Enter.
    let search = find("Search box");
    let after = page.type_text(search, "layer agent", true).await.unwrap();
    assert!(
        after["text"].as_str().unwrap().contains("You searched"),
        "{after}"
    );
    assert!(
        after["url"].as_str().unwrap().contains("q=layer+agent")
            || after["url"].as_str().unwrap().contains("q=layer%20agent"),
        "{after}"
    );

    // Click a link.
    page.navigate(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    let snap = page.snapshot().await.unwrap();
    let link = snap["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["text"] == "Go to next page")
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    let next = page.click(link).await.unwrap();
    assert_eq!(next["title"], "Second");

    // A target=_blank link is folded into the one tab the agent controls.
    page.navigate(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    let snap = page.snapshot().await.unwrap();
    let popup = snap["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["text"] == "Open in new tab")
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    let popped = page.click(popup).await.unwrap();
    assert_eq!(popped["title"], "Second", "{popped}");

    // Console and network observation, dialogs dismissed.
    page.navigate(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    let console = page.console().to_string();
    assert!(console.contains("fixture loaded"), "{console}");
    assert!(
        console.contains("hi there") && console.contains("dismissed"),
        "{console}"
    );
    let network = page.network().to_string();
    assert!(network.contains("/api/ping"), "{network}");

    // Screenshot is a real PNG.
    let png = page.screenshot(true).await.unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

    // Evaluate.
    let evaluated = page.evaluate("document.title").await.unwrap();
    assert!(
        evaluated["result"]
            .as_str()
            .unwrap()
            .contains("Fixture Home")
    );

    // Non-web schemes are refused before reaching the browser.
    assert!(page.navigate("file:///etc/passwd").await.is_err());
    manager.close_owner("owner");
    assert_eq!(manager.open_count(), 0);
}

#[tokio::test]
#[ignore = "downloads and runs a real Chromium"]
async fn proxy_blocks_unlisted_and_local_destinations() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(serve(tx));
    let port = rx.await.unwrap();
    let home = std::path::PathBuf::from(
        std::env::var("LAYERAGENT_BROWSER_TEST_HOME")
            .unwrap_or_else(|_| "/tmp/layeragent-browser-test-home".into()),
    );
    // Production manager: loopback is never reachable, even though 127.0.0.1 is listed.
    let manager = BrowserManager::with_helper(
        std::env::var_os("LAYERAGENT_BROWSER_HELPER")
            .expect("set LAYERAGENT_BROWSER_HELPER to the built CLI")
            .into(),
    );
    let session = manager.open("owner", &home, &config(), None).await.unwrap();
    let failure = session
        .page
        .navigate(&format!("http://127.0.0.1:{port}/"))
        .await;
    let reachable = failure
        .as_ref()
        .is_ok_and(|v| v["text"].as_str().unwrap_or("").contains("Welcome"));
    assert!(
        !reachable,
        "the agent's browser reached a loopback service: {failure:?}"
    );
    assert!(session.proxy_decisions().iter().any(|d| !d.allowed));
    manager.close_owner("owner");
}

#[tokio::test]
#[ignore = "downloads Chromium and needs internet"]
async fn web_search_returns_results_from_the_real_internet() {
    let home = std::path::PathBuf::from(
        std::env::var("LAYERAGENT_BROWSER_TEST_HOME")
            .unwrap_or_else(|_| "/tmp/layeragent-browser-test-home".into()),
    );
    let cfg = BrowserConfig {
        ..BrowserConfig::default()
    };
    let manager = BrowserManager::with_helper(
        std::env::var_os("LAYERAGENT_BROWSER_HELPER")
            .expect("set LAYERAGENT_BROWSER_HELPER to the built CLI")
            .into(),
    );
    let session = manager.open("owner", &home, &cfg, None).await.unwrap();
    let found = session
        .page
        .search("rust programming language", None)
        .await
        .unwrap();
    let results = found["results"].as_array().unwrap();
    assert!(!results.is_empty());
    println!("{}", serde_json::to_string_pretty(&results[0]).unwrap());
    assert!(!results[0]["title"].as_str().unwrap().is_empty());
    assert!(!results[0]["snippet"].as_str().unwrap().is_empty());
    assert!(results[0]["url"].as_str().unwrap().starts_with("http"));
    manager.close_owner("owner");
}
