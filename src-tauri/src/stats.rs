//! TODO: implemented by the stats lane. See ctx.rs for the module contract.

use std::sync::Arc;

use serde_json::Value;

use crate::ctx::Ctx;

pub fn init(_ctx: &Arc<Ctx>) -> Result<(), String> {
    Ok(())
}

pub async fn call(_ctx: &Arc<Ctx>, method: &str, _args: Value) -> Result<Value, String> {
    Err(format!("stats.{method}: not implemented"))
}

/// Called for every gateway activity update (start, progress, done).
pub fn record(_ctx: &Arc<Ctx>, _a: &crate::gateway::Activity) {}
