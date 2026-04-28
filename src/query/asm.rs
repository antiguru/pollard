//! `asm_for_function`: disassembly with per-instruction sample counts. v1 stub.

#![allow(dead_code)]

use crate::error::ToolError;
use serde::Serialize;

#[derive(Debug, Default)]
pub struct Args {
    pub function: String,
    pub module: Option<String>,
    pub with_samples: bool,
}

#[derive(Debug, Serialize)]
pub struct AsmListing {
    pub function: String,
    pub module: Option<String>,
    pub start_address: String,
    pub size: String,
    pub arch: String,
    pub instructions: Vec<AsmInstruction>,
}

#[derive(Debug, Serialize)]
pub struct AsmInstruction {
    pub offset: u32,
    pub asm: String,
    pub samples: u64,
}

#[allow(clippy::unused_async)]
pub async fn asm_for_function(_args: &Args) -> Result<AsmListing, ToolError> {
    Err(ToolError::Internal {
        message: "asm_for_function is not implemented yet (v1 stub)".to_owned(),
    })
}
