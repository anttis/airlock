//! Tests of the admin service.

mod test_admin_mcp;
mod test_admin_mcp_socket;
mod test_claude_hooks;
mod test_codex_hooks;
mod test_copilot_hooks;
#[cfg(target_os = "linux")]
mod test_execute_as_caller;
