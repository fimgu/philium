//! TODO: implemented by the terminals lane. See ctx.rs for the module contract.

use std::sync::Arc;

use serde_json::Value;

use crate::ctx::Ctx;

pub fn init(_ctx: &Arc<Ctx>) -> Result<(), String> {
    Ok(())
}

pub async fn call(_ctx: &Arc<Ctx>, method: &str, _args: Value) -> Result<Value, String> {
    Err(format!("terminals.{method}: not implemented"))
}

/// Kill or detach terminals on app exit.
pub fn shutdown(_ctx: &Arc<Ctx>) {}
