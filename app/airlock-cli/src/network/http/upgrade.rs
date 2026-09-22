//! HTTP/1.1 upgrades (WebSocket, `CONNECT`) on the h1 → h1 relay path.
//!
//! hyper's own upgrade support (`with_upgrades`, `hyper::upgrade::on`)
//! needs `Send` IO, and the RPC-backed guest transport is not. So the
//! relay lets both hyper connections finish on the switch — neither shuts
//! its socket down then — takes the sockets back with `into_parts`, and
//! relays bytes verbatim. [`Upgrade`] tracks the switch for one guest
//! connection; [`transport`] rebuilds a relay side from the parts.

use std::cell::Cell;

use hyper::body::Bytes;
use hyper::header::{CONNECTION, HeaderValue, UPGRADE};
use hyper::{Method, Request, Response, StatusCode, Version};

use super::{HyperIo, ResponseBody, text_response};
use crate::network::io;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    None,
    /// An upgrade request was forwarded; the upstream reply is pending.
    Requested,
    /// The upstream accepted the switch.
    Switched,
}

/// Upgrade progress on one guest connection, shared between the service
/// (which sees requests and replies) and the connection driver.
#[derive(Default)]
pub struct Upgrade(Cell<State>);

impl Upgrade {
    /// hyper's own test for an upgrade request (`Server::parse`): an
    /// `Upgrade` header decides on HTTP/1.1 only, else `CONNECT`. hyper ends
    /// the guest connection with `Dispatched::Upgrade` for exactly these,
    /// so the relay must agree with it.
    pub fn wants<B>(req: &Request<B>) -> bool {
        if req.headers().contains_key(UPGRADE) {
            req.version() == Version::HTTP_11
        } else {
            req.method() == Method::CONNECT
        }
    }

    /// An upgrade request is about to be forwarded. Recorded before the
    /// send: the upstream h1 connection ends the instant a 101 arrives,
    /// possibly before the service sees the reply.
    pub fn requested(&self) {
        self.0.set(State::Requested);
    }

    /// The upstream's own reply arrived, before middleware.
    pub fn upstream_replied<B>(&self, method: &Method, resp: &Response<B>) {
        if switches(method, resp.status()) {
            self.0.set(State::Switched);
        }
    }

    /// The reply is about to go to the guest, after middleware.
    ///
    /// Unless the upstream switched and the guest sees that, the guest
    /// connection ends with this reply (`Connection: close`). The upstream
    /// may already be gone, and the mirror shutdown that would normally
    /// pass that on was suppressed while the upgrade was in flight, so
    /// closing here is the one uniform answer. (This also covers an
    /// upgrade request a script denied before it was sent.) A switch
    /// status forged by a script (the upstream did not switch) becomes a
    /// 502, since there is no upstream stream to relay. A script that
    /// turned an accepted switch into something else just leaves the
    /// upstream socket to be dropped.
    pub fn reply(&self, method: &Method, resp: &mut Response<ResponseBody>) {
        match (self.switched(), switches(method, resp.status())) {
            (true, true) => return,
            (true, false) => self.0.set(State::Requested),
            (false, true) => {
                *resp = text_response(
                    StatusCode::BAD_GATEWAY,
                    "upgrade not accepted by upstream\n",
                );
            }
            (false, false) => {}
        }
        resp.headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }

    pub fn in_flight(&self) -> bool {
        self.0.get() != State::None
    }

    pub fn switched(&self) -> bool {
        self.0.get() == State::Switched
    }
}

/// hyper's client-side test (`Client::decoder`) for a reply that switches
/// the connection: 101 to anything, or a 2xx other than 204 to `CONNECT`.
fn switches(method: &Method, status: StatusCode) -> bool {
    status == StatusCode::SWITCHING_PROTOCOLS
        || (*method == Method::CONNECT && status.is_success() && status != StatusCode::NO_CONTENT)
}

/// Rebuild a relay side from a hyper IO object plus the bytes hyper had
/// already read past the end of the HTTP exchange.
pub fn transport(hyper_io: HyperIo, read_buf: Bytes) -> io::Transport {
    let (read, write) = hyper_io.into_inner().into_inner();
    io::Transport {
        read: Box::new(io::PrefixedRead::new(read_buf, read)),
        write,
        h2: false,
    }
}
