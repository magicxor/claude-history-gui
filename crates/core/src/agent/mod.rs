//! The agent protocol (`agent search|within|read|outline`): request
//! arguments ([`args`]), execution ([`execute`], [`AgentService`]) and the
//! structured response ([`response`]).

pub mod args;
pub mod cli;
#[cfg(test)]
mod cli_scenario_tests;
#[cfg(test)]
mod command_tests;
pub mod diagnostic;
pub mod protocol;
pub mod refs;
pub mod response;
#[cfg(test)]
mod response_tests;
pub mod retrieval;
pub mod sanitize;
pub mod search;
pub mod service;
#[cfg(test)]
pub mod test_support;
pub mod transcript;
pub mod visibility;

pub use args::AgentRequest;
pub use cli::{AgentCli, parse_agent_args};
pub use response::{
    AgentConversationInfo, AgentOutlineResult, AgentReadResult, AgentResponse, AgentResult,
    AgentSearchResult,
};
pub use service::{AgentNotice, AgentService, execute, execute_text};
