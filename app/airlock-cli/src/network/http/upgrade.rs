use std::cell::RefCell;

use hyper::header::{CONNECTION, UPGRADE};
use hyper::{Request, Response, StatusCode, Version};
use tokio::sync::Notify;
use tracing::debug;

use crate::network::{io, tcp};

pub(super) struct UpgradeCoordinator {
    state: RefCell<UpgradeState>,
    settled: Notify,
}

enum UpgradeState {
    Idle,
    Pending(hyper::upgrade::OnUpgrade),
    Accepted(PendingUpgrade),
}

pub(super) struct PendingUpgrade {
    container: hyper::upgrade::OnUpgrade,
    server: hyper::upgrade::OnUpgrade,
}

pub(super) enum ResponseAction {
    Forward,
    InvalidUpgrade,
}

impl Default for UpgradeCoordinator {
    fn default() -> Self {
        Self {
            state: RefCell::new(UpgradeState::Idle),
            settled: Notify::new(),
        }
    }
}

impl UpgradeCoordinator {
    pub(super) fn begin_request<B>(&self, request: &mut Request<B>) {
        if !has_upgrade_headers(request.version(), request.headers()) {
            return;
        }

        let mut state = self.state.borrow_mut();
        if matches!(*state, UpgradeState::Idle) {
            *state = UpgradeState::Pending(hyper::upgrade::on(request));
        }
    }

    pub(super) fn complete_response<B>(&self, response: &mut Response<B>) -> ResponseAction {
        let switching = response.status() == StatusCode::SWITCHING_PROTOCOLS;
        let server = if switching && has_upgrade_headers(response.version(), response.headers()) {
            response
                .extensions_mut()
                .remove::<hyper::upgrade::OnUpgrade>()
        } else {
            None
        };

        let mut state = self.state.borrow_mut();
        let current = std::mem::replace(&mut *state, UpgradeState::Idle);
        let (next, action, settled) = match (current, switching, server) {
            (UpgradeState::Pending(container), true, Some(server)) => (
                UpgradeState::Accepted(PendingUpgrade { container, server }),
                ResponseAction::Forward,
                true,
            ),
            (UpgradeState::Pending(_), true, None) => {
                (UpgradeState::Idle, ResponseAction::InvalidUpgrade, true)
            }
            (UpgradeState::Pending(_), false, _) => {
                (UpgradeState::Idle, ResponseAction::Forward, true)
            }
            (state, true, _) => (state, ResponseAction::InvalidUpgrade, false),
            (state, false, _) => (state, ResponseAction::Forward, false),
        };
        *state = next;
        drop(state);

        if settled {
            self.settled.notify_one();
        }
        action
    }

    pub(super) fn cancel_attempt(&self) {
        let mut state = self.state.borrow_mut();
        if matches!(*state, UpgradeState::Pending(_)) {
            *state = UpgradeState::Idle;
            self.settled.notify_one();
        }
    }

    pub(super) async fn wait_until_settled(&self) {
        loop {
            let notified = self.settled.notified();
            if !matches!(*self.state.borrow(), UpgradeState::Pending(_)) {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn take_accepted(&self) -> Option<PendingUpgrade> {
        let mut state = self.state.borrow_mut();
        let current = std::mem::replace(&mut *state, UpgradeState::Idle);
        match current {
            UpgradeState::Accepted(upgrade) => Some(upgrade),
            current => {
                *state = current;
                None
            }
        }
    }
}

impl PendingUpgrade {
    pub(super) async fn relay(self) -> anyhow::Result<()> {
        let (container, server) = tokio::try_join!(self.container, self.server)?;
        debug!("HTTP upgrade complete, starting raw relay");
        tcp::relay(upgraded_transport(container), upgraded_transport(server)).await;
        Ok(())
    }
}

fn upgraded_transport(upgraded: hyper::upgrade::Upgraded) -> io::Transport {
    let (read, write) = tokio::io::split(hyper_util::rt::TokioIo::new(upgraded));
    io::Transport {
        read: Box::new(read),
        write: Box::new(write),
        h2: false,
    }
}

/// HTTP/1.1 upgrade messages need both hop-by-hop headers. Hyper leaves
/// validating the application protocol named by `Upgrade` to the endpoints.
fn has_upgrade_headers(version: Version, headers: &hyper::HeaderMap) -> bool {
    version == Version::HTTP_11
        && headers.contains_key(UPGRADE)
        && headers
            .get_all(CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
}
