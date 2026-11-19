//! `usagestat mcp`: a Model Context Protocol server on stdio, so agents can ask
//! about their own quotas and costs. It runs only while an agent has it open,
//! reads the running daemon, and never writes anything.
//!
//! Messages are newline-delimited JSON-RPC 2.0 on stdin and stdout. Nothing
//! else may reach stdout; diagnostics go to stderr.
use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::net::SocketAddr;

use crate::{project, sessions, statusline, whatif};

/// Newest first; an unknown client version gets the newest.
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub struct Server {
    bind: Option<SocketAddr>,
    budgets: Vec<usagestat_core::budgets::BudgetConfig>,
    protocol: &'static str,
}

pub fn run(
    bind: Option<SocketAddr>,
    budgets: Vec<usagestat_core::budgets::BudgetConfig>,
) -> Result<()> {
    let mut server = Server {
        bind,
        budgets,
        protocol: PROTOCOL_VERSIONS[0],
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle_line(&line) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

impl Server {
    /// One incoming line to at most one outgoing line.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(_) => return Some(error(Value::Null, -32700, "Parse error").to_string()),
        };
        // Batches were removed from the protocol in 2025-06-18; older clients
        // may still send them.
        if let Value::Array(batch) = message {
            let replies: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(m)).collect();
            return (!replies.is_empty()).then(|| Value::Array(replies).to_string());
        }
        self.handle(message).map(|reply| reply.to_string())
    }

    fn handle(&mut self, message: Value) -> Option<Value> {
        let method = message["method"].as_str().unwrap_or_default().to_string();
        let id = message.get("id").cloned();
        // Notifications (no id), such as notifications/initialized, get no reply.
        let id = id?;
        if message["jsonrpc"] != "2.0" || method.is_empty() {
            return Some(error(id, -32600, "Invalid Request"));
        }
        let params = &message["params"];
        let result = match method.as_str() {
            "initialize" => {
                let requested = params["protocolVersion"].as_str().unwrap_or_default();
                self.protocol = PROTOCOL_VERSIONS
                    .iter()
                    .find(|v| **v == requested)
                    .copied()
                    .unwrap_or(PROTOCOL_VERSIONS[0]);
                json!({
                    "protocolVersion": self.protocol,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "usagestat", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Read-only AI agent usage from the local usagestat daemon: quotas and resets, cost per repository and git branch, costliest sessions, and model what-ifs. Costs are API-equivalent estimates from local logs, not invoices.",
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => self.call(params),
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn call(&self, params: &Value) -> Value {
        let args = &params["arguments"];
        let text = |key: &str| {
            args[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let number = |key: &str, max: u64| args[key].as_u64().filter(|n| *n <= max);
        let outcome = match params["name"].as_str().unwrap_or_default() {
            "quota_status" => {
                let providers: Vec<String> = args["providers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p.as_str().map(str::to_owned))
                    .take(64)
                    .collect();
                statusline::quotas(self.bind, &providers)
            }
            "project_usage" => project::fetch(
                text("path").as_deref().map(std::path::Path::new),
                text("project"),
                &self.budgets,
                self.bind,
            ),
            "costly_sessions" => sessions::fetch(sessions::Options {
                limit: number("limit", 100).unwrap_or(10).max(1) as usize,
                days: match number("days", 3650) {
                    Some(0) => None,
                    Some(days) => Some(days as i64),
                    None => Some(30),
                },
                project: text("project"),
                bind: self.bind,
                json: true,
            }),
            "model_what_if" => match text("model") {
                Some(model) => whatif::fetch(whatif::Options {
                    model,
                    provider: text("source"),
                    from: text("from"),
                    days: match number("days", 3650) {
                        Some(0) => None,
                        Some(days) => Some(days as u32),
                        None => Some(30),
                    },
                    bind: self.bind,
                    json: true,
                }),
                None => Err(anyhow::anyhow!("model is required")),
            },
            other => Err(anyhow::anyhow!("unknown tool `{other}`")),
        };
        match outcome {
            Ok(value) => {
                let mut result = json!({
                    "content": [{"type": "text", "text": value.to_string()}],
                    "isError": false,
                });
                // structuredContent must be an object; it arrived in 2025-06-18.
                if self.protocol >= "2025-06-18" {
                    result["structuredContent"] = if value.is_object() {
                        value
                    } else {
                        json!({"items": value})
                    };
                }
                result
            }
            Err(e) => json!({
                "content": [{"type": "text", "text": format!("{e:#}")}],
                "isError": true,
            }),
        }
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tools() -> Value {
    let read_only = json!({"readOnlyHint": true, "openWorldHint": false});
    json!([
        {
            "name": "quota_status",
            "title": "Quota status",
            "description": "Current quota windows for AI coding agents and providers: percent used, reset time, forecast run-out time if on pace to run out before reset, and today's cost.",
            "inputSchema": {"type": "object", "properties": {
                "providers": {"type": "array", "items": {"type": "string"}, "description": "Provider ids such as claude or codex. Empty: every provider that reports quotas."}
            }},
            "annotations": read_only,
        },
        {
            "name": "project_usage",
            "title": "Repository cost",
            "description": "API-equivalent cost and tokens for one repository from Claude Code, Codex, Gemini CLI and Amp logs: today, 7 days, 30 days, all time, per agent; cost per git branch; and its monthly budget if configured. Worktrees and subfolders count toward the main repository.",
            "inputSchema": {"type": "object", "properties": {
                "path": {"type": "string", "description": "A directory inside the repository. Defaults to the server's working directory."},
                "project": {"type": "string", "description": "Project name instead of a path."}
            }},
            "annotations": read_only,
        },
        {
            "name": "costly_sessions",
            "title": "Costliest sessions",
            "description": "The agent sessions that cost the most, with agent, project, git branch, main model and duration.",
            "inputSchema": {"type": "object", "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Default 10."},
                "days": {"type": "integer", "minimum": 0, "maximum": 3650, "description": "Sessions active in the last N days. Default 30; 0 for all time."},
                "project": {"type": "string", "description": "Only sessions in this project."}
            }},
            "annotations": read_only,
        },
        {
            "name": "model_what_if",
            "title": "Model what-if",
            "description": "Recost recent local usage as if it had run on another model, at base API rates for the same token counts. A rough guide, not a prediction.",
            "inputSchema": {"type": "object", "required": ["model"], "properties": {
                "model": {"type": "string", "description": "Target model id, such as claude-sonnet-5-5."},
                "source": {"type": "string", "enum": ["claude", "codex", "gemini", "amp"], "description": "Usage to recost. Defaults to the target model's vendor."},
                "from": {"type": "string", "description": "Only usage that ran on this model."},
                "days": {"type": "integer", "minimum": 0, "maximum": 3650, "description": "Default 30; 0 for all time."}
            }},
            "annotations": read_only,
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        // Port 9 (discard) has no daemon, so tool calls fail fast.
        Server {
            bind: Some("127.0.0.1:9".parse().unwrap()),
            budgets: Vec::new(),
            protocol: PROTOCOL_VERSIONS[0],
        }
    }

    fn reply(server: &mut Server, message: Value) -> Value {
        serde_json::from_str(&server.handle_line(&message.to_string()).unwrap()).unwrap()
    }

    #[test]
    fn handshake_negotiates_the_protocol_and_lists_read_only_tools() {
        let mut s = server();
        let init = reply(
            &mut s,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "usagestat");
        assert!(
            s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none()
        );
        let list = reply(
            &mut s,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        );
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        assert!(
            tools
                .iter()
                .all(|t| t["annotations"]["readOnlyHint"] == true)
        );
        let unknown = reply(
            &mut s,
            json!({"jsonrpc":"2.0","id":"x","method":"initialize","params":{"protocolVersion":"1999-01-01"}}),
        );
        assert_eq!(unknown["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn errors_are_json_rpc_errors_or_tool_errors() {
        let mut s = server();
        assert_eq!(
            serde_json::from_str::<Value>(&s.handle_line("{nope").unwrap()).unwrap()["error"]["code"],
            -32700
        );
        let missing = reply(
            &mut s,
            json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}),
        );
        assert_eq!(missing["error"]["code"], -32601);
        let offline = reply(
            &mut s,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"quota_status","arguments":{}}}),
        );
        assert_eq!(offline["result"]["isError"], true);
        let no_model = reply(
            &mut s,
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"model_what_if","arguments":{}}}),
        );
        assert_eq!(
            no_model["result"]["content"][0]["text"],
            "model is required"
        );
    }
}
