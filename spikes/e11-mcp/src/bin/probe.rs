//! Probe: what rmcp does by default with its own macros. Not the server; see main.rs.

use rmcp::{
    ServiceExt, handler::server::wrapper::Parameters, schemars, tool, tool_router, transport::stdio,
};
use serde::Deserialize;

#[derive(Deserialize, schemars::JsonSchema)]
struct SendArgs {
    to: String,
    body: String,
    since: Option<u64>,
}

struct Probe;

#[tool_router(server_handler)]
impl Probe {
    #[tool(description = "Probe.")]
    async fn send(&self, Parameters(args): Parameters<SendArgs>) -> String {
        format!("{} {} {:?}", args.to, args.body, args.since)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    Probe.serve(stdio()).await?.waiting().await?;
    Ok(())
}
