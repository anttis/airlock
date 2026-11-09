//! Tools of the `airlock` MCP server.
//!
//! Tools: run a command with extended network access, show the network
//! rules, add or change a rule, and read the user manual. The server lists
//! only the tools of the features that the host enabled. Checks the tool
//! arguments and sends each valid request to the grant task.
//! The handlers can run on any thread.
//!
//! Each connection has its client identity. A command runs as the user of a
//! socket client. Over the old HTTP endpoint, the command tool is
//! deprecated and runs commands as the sandbox user.

mod tool_execute;
mod tool_get_manual_entry;
mod tool_list_manual_entries;
mod tool_show_rules;
mod tool_upsert_rule;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool_handler};

use super::bridge::GrantBridge;
use super::instructions::instructions;
use crate::admin::caller::{Caller, PeerId};
use crate::admin::capabilities::{Capabilities, RuleEditing};

/// Handler of the `airlock` MCP server.
#[derive(Clone)]
pub struct AdminTools {
    grants: Option<GrantBridge>,
    caps: Capabilities,
    caller: Caller,
    router: ToolRouter<Self>,
}

impl AdminTools {
    /// Create the handler of the old HTTP endpoint, with the tools of the
    /// enabled features. Its commands run as the sandbox user.
    /// Args:
    ///  - `grants`: Handle to the grant task, or `None` if the host network
    ///    service is not available
    ///  - `caps`: Agent features that the host enabled
    pub fn new(grants: Option<GrantBridge>, caps: Capabilities) -> Self {
        Self::build(grants, caps, Caller::BootUnverified)
    }

    /// Create the handler of one socket connection. Its commands run as
    /// the user of the client.
    /// Args:
    ///  - `grants`, `caps`: Same as in [`AdminTools::new`]
    ///  - `peer`: IDs of the client from the kernel
    pub fn for_caller(grants: Option<GrantBridge>, caps: Capabilities, peer: PeerId) -> Self {
        Self::build(grants, caps, Caller::Verified(peer))
    }

    /// Create the handler for a client. See [`AdminTools::new`].
    fn build(grants: Option<GrantBridge>, caps: Capabilities, caller: Caller) -> Self {
        let mut router = Self::list_manual_entries_router() + Self::get_manual_entry_router();
        if caps.grants {
            router += Self::execute_router();
            if caller == Caller::BootUnverified
                && let Some(route) = router.map.get_mut("execute")
            {
                route.attr.description = Some(tool_execute::http_description().into());
            }
        }
        if caps.rule_editing != RuleEditing::None {
            router += Self::show_rules_router() + Self::upsert_rule_router();
        }
        Self {
            grants,
            caps,
            caller,
            router,
        }
    }

    /// Get the grant task handle.
    ///
    /// Returns:
    ///   The handle, or an error result if the host network service is not
    ///   available.
    fn grants(&self) -> Result<&GrantBridge, CallToolResult> {
        self.grants.as_ref().ok_or_else(|| {
            tool_error("The airlock network tools are not available in this sandbox.")
        })
    }
}

#[tool_handler(router = self.router)]
impl ServerHandler for AdminTools {
    fn get_info(&self) -> ServerConfig {
        let info = Implementation::new("airlock", env!("CARGO_PKG_VERSION"));
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(info)
            .with_instructions(instructions(self.caps, ""))
    }
}

/// Make an error result with a text for the agent.
pub(super) fn tool_error(text: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(text)])
}
