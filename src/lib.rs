pub mod agentic;
pub mod agentic_handoff;
mod auth;
pub mod autonomy;
pub mod mcp;
pub mod policy;
pub mod receipt;
pub mod setup;
pub mod strategy;
pub mod trade;

pub use auth::{sign_request_at, BinanceWeb3Transport};
pub use mcp::FlowBnbMcpServer;
pub use policy::{
    Evaluation, ExecutionMode, SimulationStatus, TradeIntent, TradePolicy, Violation,
};

pub mod demo;
pub mod handoff;
pub mod wallet;
