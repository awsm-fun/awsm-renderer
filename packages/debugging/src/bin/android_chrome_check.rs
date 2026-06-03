use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Deserialize)]
struct ChromeTarget {
    #[serde(rename = "type")]
    target_type: Option<String>,
    url: Option<String>,
    #[serde(rename = "webSocketDebuggerUrl")]
    websocket_debugger_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let port = std::env::var("PORT")
        .expect("PORT environment variable must be set to identify the Chrome tab to debug");
    let target_url_part = format!("localhost:{port}");
    let wait_ms: u64 = std::env::var("WAIT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60_000);

    let target = find_target(&target_url_part).await?;
    let ws_url = target
        .websocket_debugger_url
        .ok_or_else(|| anyhow!("Target did not include webSocketDebuggerUrl"))?;

    let failure_patterns = [
        Regex::new("(?i)\\bERROR\\b")?,
        Regex::new("(?i)VK_ERROR_INITIALIZATION_FAILED")?,
        Regex::new("(?i)CreateComputePipelines failed")?,
        Regex::new("(?i)CreateGraphicsPipelines failed")?,
        Regex::new("(?i)Error initializing renderer")?,
        Regex::new("(?i)Error initializing Renderer")?,
        Regex::new("(?i)PipelineCreation")?,
    ];

    let success_patterns = [Regex::new("(?i)\\[scene\\] model loaded")?];

    let (mut ws, _) = connect_async(&ws_url)
        .await
        .with_context(|| format!("Failed to connect to Chrome DevTools WebSocket: {ws_url}"))?;

    let mut next_id = 1_u64;

    send_cdp(&mut ws, &mut next_id, "Runtime.enable", json!({})).await?;
    send_cdp(&mut ws, &mut next_id, "Log.enable", json!({})).await?;
    send_cdp(&mut ws, &mut next_id, "Page.enable", json!({})).await?;

    // Reload the real Android Chrome tab.
    send_cdp(
        &mut ws,
        &mut next_id,
        "Page.reload",
        json!({ "ignoreCache": true }),
    )
    .await?;

    let deadline = sleep(Duration::from_millis(wait_ms));
    tokio::pin!(deadline);

    let mut logs: Vec<String> = Vec::new();
    let mut load_seen = false;

    loop {
        tokio::select! {
            _ = &mut deadline => {
                print_logs(&logs);

                if !load_seen {
                    eprintln!("\nPage load event was not observed.");
                    std::process::exit(3);
                }

                eprintln!(
                    "\nTimed out after {wait_ms}ms waiting for either a failure log or `[scene] model loaded`."
                );
                std::process::exit(4);
            }

            maybe_msg = timeout(Duration::from_millis(500), ws.next()) => {
                match maybe_msg {
                    Ok(Some(Ok(Message::Text(text)))) => {
                        handle_cdp_message(&text, &mut logs, &mut load_seen);

                        let joined = logs.join("\n");

                        if failure_patterns.iter().any(|re| re.is_match(&joined)) {
                            print_logs(&logs);
                            eprintln!("\nDetected renderer/WebGPU/Vulkan failure.");
                            std::process::exit(1);
                        }

                        if success_patterns.iter().any(|re| re.is_match(&joined)) {
                            print_logs(&logs);
                            println!("\nDetected successful model load.");
                            std::process::exit(0);
                        }
                    }
                    Ok(Some(Ok(Message::Close(_)))) => {
                        print_logs(&logs);
                        eprintln!("\nChrome DevTools WebSocket closed before success or failure was observed.");
                        std::process::exit(5);
                    }
                    Ok(Some(Ok(_))) => {}
                    Ok(Some(Err(err))) => return Err(err).context("WebSocket read failed"),
                    Ok(None) => {
                        print_logs(&logs);
                        eprintln!("\nChrome DevTools WebSocket ended before success or failure was observed.");
                        std::process::exit(5);
                    }
                    Err(_) => {}
                }
            }
        }
    }
}

async fn find_target(target_url_part: &str) -> Result<ChromeTarget> {
    let targets: Vec<ChromeTarget> = reqwest::get("http://127.0.0.1:9222/json/list")
        .await
        .context("Could not query http://127.0.0.1:9222/json/list. Did you run `adb forward tcp:9222 localabstract:chrome_devtools_remote`?")?
        .json()
        .await
        .context("Could not parse Chrome target list JSON")?;

    targets
        .into_iter()
        .find(|target| {
            target.target_type.as_deref() == Some("page")
                && target
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains(target_url_part))
                && target.websocket_debugger_url.is_some()
        })
        .ok_or_else(|| {
            anyhow!(
                "Could not find Android Chrome tab matching `{}`. Open http://{target_url_part} on the phone and retry.",
                target_url_part
            )
        })
}

async fn send_cdp<S>(ws: &mut S, next_id: &mut u64, method: &str, params: Value) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let id = *next_id;
    *next_id += 1;

    let payload = json!({
        "id": id,
        "method": method,
        "params": params,
    });

    ws.send(Message::Text(payload.to_string().into()))
        .await
        .with_context(|| format!("Failed to send CDP method {method}"))?;

    Ok(())
}

fn handle_cdp_message(text: &str, logs: &mut Vec<String>, load_seen: &mut bool) {
    let Ok(msg) = serde_json::from_str::<Value>(text) else {
        return;
    };

    match msg.get("method").and_then(Value::as_str) {
        Some("Page.loadEventFired") => {
            *load_seen = true;
        }

        Some("Runtime.consoleAPICalled") => {
            let params = &msg["params"];
            let kind = params["type"].as_str().unwrap_or("log");

            let args = params["args"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(cdp_remote_object_to_string)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();

            logs.push(format!("[console.{kind}] {args}"));
        }

        Some("Runtime.exceptionThrown") => {
            let details = &msg["params"]["exceptionDetails"];
            let text = details["text"].as_str().unwrap_or("exception");
            logs.push(format!("[exception] {text}"));
            logs.push(details.to_string());
        }

        Some("Log.entryAdded") => {
            let entry = &msg["params"]["entry"];
            let level = entry["level"].as_str().unwrap_or("unknown");
            let text = entry["text"].as_str().unwrap_or("");
            logs.push(format!("[log.{level}] {text}"));
        }

        _ => {}
    }
}

fn cdp_remote_object_to_string(value: &Value) -> String {
    if let Some(s) = value.get("value").and_then(Value::as_str) {
        return s.to_string();
    }

    if let Some(v) = value.get("value") {
        return v.to_string();
    }

    if let Some(s) = value.get("description").and_then(Value::as_str) {
        return s.to_string();
    }

    String::new()
}

fn print_logs(logs: &[String]) {
    println!("=== Android Chrome captured logs ===");
    if logs.is_empty() {
        println!("(no console/log output captured)");
    } else {
        for line in logs {
            println!("{line}");
        }
    }
}
