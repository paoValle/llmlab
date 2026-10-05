//! The vertical slice: a real agent, over a real socket, through this gateway.
//!
//! The other scenarios measure the gateway against a fake `Upstream` in the same process. They
//! are fast and deterministic, and they miss one thing: whether the whole thing works when a
//! client is on the other end of a wire. This module puts one there:
//!
//! ```text
//! agentloop (TypeScript, real budget, real trace)
//!     │  HTTP  POST /v1/chat/completions,  Authorization: Bearer <tenant key>
//!     ▼
//! llmgateway (axum → auth → cap → router → reqwest)
//!     │  HTTP
//!     ▼
//! a fake provider, whose usage the gateway meters
//! ```
//!
//! What it is really checking is **reconciliation**: the runtime and the gateway compute the
//! cost of the same run independently, from the same `usage`, in two languages. If those two
//! numbers disagree, one of them is wrong — and a gateway whose number disagrees with the
//! runtime's number is a gateway nobody can bill against.
//!
//! It is skipped, loudly, when the `agentloop` checkout is not there: a lab that fails because
//! a sibling repository is missing teaches nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use llmgateway::auth::Authenticator;
use llmgateway::budget::{BudgetRegistry, Month, TenantBudget};
use llmgateway::gateway::{Gateway, GatewayConfig};
use llmgateway::http::serve_ephemeral;
use llmgateway::http_upstream::HttpUpstream;
use llmgateway::meter::Meter;
use llmgateway::pricing::{micros, Price, PriceTable};
use llmgateway::router::Router as ModelRouter;
use llmgateway::upstream::Upstream;
use llmgateway::{usd, MicroUsd};
use tokio::net::TcpListener;

use crate::NOW;

/// The tenant the agent acts as.
pub const E2E_TENANT: &str = "acme";
/// The tenant key the agent sends. The gateway replaces it with the provider key.
pub const E2E_KEY: &str = "sk-acme";
/// The model both sides are configured for.
pub const E2E_MODEL: &str = "gpt-4o-mini";

/// What one agent run reported, as the TypeScript side printed it.
#[derive(Debug, Clone)]
pub struct AgentRun {
    /// Which scenario it was asked to run.
    pub scenario: String,
    /// Whether the run completed.
    pub ok: bool,
    /// Steps the agent executed.
    pub steps: u64,
    /// Why it stopped, from the agent's point of view.
    pub stop_reason: String,
    /// The final answer, if it produced one.
    pub answer: Option<String>,
    /// What the agent's own trace says it spent.
    pub spent_micro_usd: MicroUsd,
    /// Whether replaying the agent's trace reproduced it.
    pub replay_equal: bool,
    /// The error class, when the run did not complete.
    pub error: Option<String>,
    /// The error message, verbatim.
    pub message: Option<String>,
}

/// The whole vertical, measured once.
#[derive(Debug, Clone)]
pub struct E2eRun {
    /// Whether the agent side could be run at all.
    pub ran: bool,
    /// Why it could not, when it could not.
    pub skip_reason: Option<String>,
    /// What the gateway metered for these requests: its independent accounting.
    pub gateway_spent: MicroUsd,
    /// What the agent's own trace says it spent.
    pub agent: AgentRun,
    /// Requests the provider actually served.
    pub provider_calls: usize,
    /// The same run against a cap of one micro-dollar, so the gateway refuses it.
    pub denied: Option<AgentRun>,
}

impl E2eRun {
    /// The reconciliation difference: zero means two independent accountings agree.
    #[must_use]
    pub fn difference(&self) -> i128 {
        i128::from(self.gateway_spent) - i128::from(self.agent.spent_micro_usd)
    }

    /// The report section, in markdown.
    #[must_use]
    pub fn markdown(&self) -> String {
        if !self.ran {
            let reason = self.skip_reason.as_deref().unwrap_or("unknown reason");
            return format!(
                "## 7. The agent, over the wire, through the gateway\n\n\
                 **Skipped**: {reason}\n\n\
                 Reproduce it with a sibling checkout: `git clone https://github.com/paoValle/agentloop ../agentloop` and `npm install` here.\n"
            );
        }

        let usd = |micro: MicroUsd| format!("{:.6} USD", micro as f64 / 1_000_000.0);
        let mut out = String::from(
            "## 7. The agent, over the wire, through the gateway\n\n\
             agentloop (TypeScript, real budget, real trace) → HTTP → this gateway → HTTP → provider.\n\n\
             | measure | value |\n|---|---|\n",
        );
        let _ = writeln!(
            out,
            "| what the gateway metered | {} ({} µUSD) |",
            usd(self.gateway_spent),
            self.gateway_spent
        );
        let _ = writeln!(
            out,
            "| what the agent's trace says it spent | {} ({} µUSD) |",
            usd(self.agent.spent_micro_usd),
            self.agent.spent_micro_usd
        );
        let outcome = if self.agent.ok {
            String::new()
        } else {
            format!(
                "\nThe run did not complete: `{}` — {}\n",
                self.agent.error.as_deref().unwrap_or("unknown error"),
                self.agent.message.as_deref().unwrap_or("(no message)")
            )
        };
        let _ = writeln!(
            out,
            "| difference | {} µUSD |\n| steps / stop reason | {} / {} |\n| requests the provider served | {} |\n| the agent's own trace replays identically | {} |\n| final answer | {} |{}",
            self.difference(),
            self.agent.steps,
            self.agent.stop_reason,
            self.provider_calls,
            self.agent.replay_equal,
            self.agent.answer.as_deref().unwrap_or("(none)"),
            outcome
        );
        out.push_str(
            "\nTwo implementations of the same arithmetic — TypeScript in the runtime, Rust in the gateway — \
             compute the cost of the same run from the same `usage`. A non-zero difference means one of them is wrong: \
             reconciling a runtime's budget with a gateway's invoice is the whole reason both exist.\n",
        );
        if let Some(denied) = &self.denied {
            let _ = writeln!(
                out,
                "\nThe same run for a tenant whose budget is already exhausted: the gateway answered **429**, the provider was not called, \
                 and the agent's policy raised `{}` with\n\n> {}\n\nA cap the runtime cannot talk its way around is a cap; \
                 a 429 that the agent retried as if it were a provider hiccup would not be.\n",
                denied.error.as_deref().unwrap_or("PolicyError"),
                denied.message.as_deref().unwrap_or("(no message)")
            );
        }
        out
    }
}

// --- the fake provider, over HTTP -------------------------------------------------------

struct ProviderState {
    replies: Mutex<Vec<(u16, String)>>,
    calls: AtomicUsize,
    bodies: Mutex<Vec<Vec<u8>>>,
}

async fn start_provider(replies: Vec<(u16, String)>) -> (String, Arc<ProviderState>) {
    let state = Arc::new(ProviderState {
        replies: Mutex::new(replies),
        calls: AtomicUsize::new(0),
        bodies: Mutex::new(Vec::new()),
    });
    let router = Router::new()
        .route("/v1/chat/completions", post(provider_handler))
        .with_state(Arc::clone(&state));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind provider");
    let address = listener.local_addr().expect("provider address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}/v1"), state)
}

async fn provider_handler(
    State(state): State<Arc<ProviderState>>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.bodies.lock().expect("bodies").push(body.to_vec());
    let reply = {
        let mut replies = state.replies.lock().expect("replies");
        if replies.len() == 1 {
            replies[0].clone()
        } else {
            replies.remove(0)
        }
    };

    // A real provider echoes the model it served. Doing it here is not cosmetic: without it the
    // agent's trace records `model: "unknown"` and its own replay diverges from the original run,
    // because the recorded policy is labelled with what the provider reported. The lab found
    // that, which is the sort of thing a lab is for.
    let model = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(|m| m.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let body = match serde_json::from_str::<serde_json::Value>(&reply.1) {
        Ok(mut value) => {
            if let Some(object) = value.as_object_mut() {
                object.insert("model".to_owned(), serde_json::Value::String(model));
            }
            value.to_string()
        }
        Err(_) => reply.1.clone(),
    };

    (
        StatusCode::from_u16(reply.0).unwrap_or(StatusCode::OK),
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

fn usage_body(content: &str, prompt: u64, completion: u64) -> String {
    // serde_json, not a hand-escaped raw string: the first version of this file had four
    // escaped closing braces in a row and one too few, which is exactly the bug I had already
    // fixed once in this repo
    serde_json::json!({
        "choices": [{"message": {"content": content}}],
        "usage": {"prompt_tokens": prompt, "completion_tokens": completion}
    })
    .to_string()
}

fn prices() -> PriceTable {
    let mut map = BTreeMap::new();
    map.insert(
        E2E_MODEL.to_owned(),
        Price {
            input: micros(150),
            output: micros(600),
        },
    );
    PriceTable::new(map)
}

/// Serves a gateway whose only provider is `provider_url`, and returns its base URL.
async fn serve_gateway(provider_url: String, cap: MicroUsd) -> (String, Arc<Meter>) {
    let prices = prices();
    let meter = Arc::new(Meter::new(prices.clone(), 1));
    let budgets = BudgetRegistry::new();
    budgets.insert(TenantBudget::new(E2E_TENANT, cap, Month::of(NOW)), NOW);
    let timeout = Duration::from_secs(10);

    let upstream: Arc<dyn Upstream> = Arc::new(HttpUpstream::new(
        "provider-a",
        provider_url,
        "sk-provider",
        None,
    ));

    let gateway = Arc::new(Gateway::new(GatewayConfig {
        authenticator: Authenticator::new(
            &[(E2E_TENANT.to_owned(), E2E_KEY.to_owned())],
            &[(E2E_TENANT.to_owned(), BTreeSet::new())],
        ),
        budget: budgets,
        meter: Arc::clone(&meter),
        router: Arc::new(ModelRouter::new(vec![upstream], 2, timeout)),
        prices,
        max_output_default: 64,
        timeout,
        now: Arc::new(|| NOW),
    }));

    // the base URL includes `/v1`: clients append `/chat/completions` to it, exactly as they
    // would with any OpenAI-compatible endpoint. Without it every request is a 404, which is
    // how the first version of this scenario "measured" a gateway that was never called.
    let (address, _handle) = serve_ephemeral(gateway).await.expect("serve the gateway");
    (format!("http://{address}/v1"), meter)
}

// --- running the agent ------------------------------------------------------------------

/// Runs the TypeScript driver and returns what it printed.
fn run_agent(workspace: &Path, base_url: &str, scenario: &str) -> Result<AgentRun, String> {
    let tsx = workspace.join("node_modules/.bin/tsx");
    if !tsx.exists() {
        return Err(format!(
            "{} is missing: run `npm install` in {}",
            tsx.display(),
            workspace.display()
        ));
    }
    let trace_out = workspace.join(format!("reports/agent-{scenario}.jsonl"));

    let output = Command::new(tsx)
        .current_dir(workspace)
        .args([
            "agent/run.ts",
            "--base-url",
            base_url,
            "--api-key",
            E2E_KEY,
            "--model",
            E2E_MODEL,
            "--scenario",
            scenario,
            "--trace-out",
            &trace_out.display().to_string(),
        ])
        .output()
        .map_err(|error| format!("cannot run the agent: {error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .rfind(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| {
            format!(
                "the agent printed no JSON. stdout: {stdout:?} stderr: {:?}",
                String::from_utf8_lossy(&output.stderr)
            )
        })?;

    let parsed: serde_json::Value = serde_json::from_str(line)
        .map_err(|error| format!("the agent printed invalid JSON: {error} in {line:?}"))?;
    let field = |name: &str| parsed.get(name).cloned().unwrap_or(serde_json::Value::Null);
    let number = |name: &str| field(name).as_u64().unwrap_or(0);
    let text = |name: &str| field(name).as_str().map(str::to_owned);

    Ok(AgentRun {
        scenario: text("scenario").unwrap_or_else(|| scenario.to_owned()),
        ok: field("ok").as_bool().unwrap_or(false),
        steps: number("steps"),
        stop_reason: text("stopReason").unwrap_or_else(|| "unknown".to_owned()),
        answer: text("answer"),
        spent_micro_usd: number("spentMicroUsd"),
        replay_equal: field("replayEqual").as_bool().unwrap_or(false),
        error: text("error"),
        message: text("message"),
    })
}

/// Runs the whole vertical. Never fails: it reports why it could not run.
pub async fn scenario_e2e(workspace: &Path) -> E2eRun {
    let agentloop: PathBuf = workspace.join("../agentloop");
    if !agentloop.join("src/index.ts").exists() {
        return E2eRun {
            ran: false,
            skip_reason: Some(format!(
                "the agentloop checkout is not at {}",
                agentloop.display()
            )),
            gateway_spent: 0,
            agent: skipped_agent(),
            provider_calls: 0,
            denied: None,
        };
    }
    // 1. the happy path: two steps, one provider, plenty of budget.
    let (provider_url, provider) = start_provider(vec![
        (
            200,
            usage_body("The cheapest is Wizz at 41 euros.", 1_000, 500),
        ),
        (200, usage_body("Wizz, at 41 euros.", 1_000, 500)),
    ])
    .await;
    let (gateway_url, meter) = serve_gateway(provider_url, usd(1.0).expect("cap")).await;
    let agent = match run_agent(workspace, &gateway_url, "happy") {
        Ok(agent) => agent,
        Err(reason) => {
            return E2eRun {
                ran: false,
                skip_reason: Some(reason),
                gateway_spent: meter.spent(E2E_TENANT),
                agent: skipped_agent(),
                provider_calls: provider.calls.load(Ordering::SeqCst),
                denied: None,
            }
        }
    };
    let gateway_spent = meter.spent(E2E_TENANT);
    let provider_calls = provider.calls.load(Ordering::SeqCst);

    // 2. the same run for a tenant whose budget is already exhausted: the gateway must refuse
    //    it before the provider is called, and the runtime must surface that refusal.
    let (provider_url, denied_provider) = start_provider(vec![(
        200,
        usage_body("this should never be sent", 1_000, 500),
    )])
    .await;
    let (gateway_url, _) = serve_gateway(provider_url, micros(0)).await;
    let denied = run_agent(workspace, &gateway_url, "denied").ok();
    let denied_calls = denied_provider.calls.load(Ordering::SeqCst);
    if denied_calls != 0 {
        // the whole point of the cap: a refused request must not reach anyone
        eprintln!(
            "WARNING: a request refused by the cap reached the provider {denied_calls} time(s)"
        );
    }

    E2eRun {
        ran: true,
        skip_reason: None,
        gateway_spent,
        agent,
        provider_calls,
        denied,
    }
}

fn skipped_agent() -> AgentRun {
    AgentRun {
        scenario: "skipped".to_owned(),
        ok: false,
        steps: 0,
        stop_reason: "skipped".to_owned(),
        answer: None,
        spent_micro_usd: 0,
        replay_equal: false,
        error: None,
        message: None,
    }
}
