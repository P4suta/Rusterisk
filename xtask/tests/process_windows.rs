#![cfg(windows)]

#[path = "process_windows/early_leader.rs"]
mod early_leader;
#[path = "owned_command/windows_output.rs"]
mod output;
#[path = "process_windows/owned_interrupt.rs"]
mod owned_interrupt;
#[path = "process_windows/owned_projection.rs"]
mod owned_projection;
#[path = "process_windows/owned_service.rs"]
mod owned_service;
