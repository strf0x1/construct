//! What the running hub does for each control-socket request (#360, #361).
//!
//! Each operation changes the registry under the hub's one lock, saves it,
//! and hands any effect on a live link (approve, deny, revoke) to that link's
//! task as a `Verdict`. The design choice: approval exists only here, never
//! offline, because the SAS code a human compares lives only in an open link.

use crate::admit::Live;
use crate::control::{Request, Response};
use crate::invite::{self, InviteReply};
use crate::state::{Shared, Verdict, now};
use anyhow::{Context, Result, bail};
use tokio::sync::watch;

impl Shared {
    /// Answer one request.
    pub(crate) fn handle(&mut self, req: Request, stop: &watch::Sender<bool>) -> Response {
        match req {
            Request::Status => Response::status(self.status()),
            Request::Stop => {
                let _ = stop.send(true);
                Response::ok()
            }
            Request::Invite { id, ttl_secs, url } => match self.invite(&id, ttl_secs, url) {
                Ok(reply) => Response::invite(reply),
                Err(e) => Response::error(format!("{e:#}")),
            },
            Request::Approve { id } => Response::of(self.approve(&id)),
            Request::Deny { id } => Response::of(self.deny(&id)),
            Request::Revoke { id } => Response::of(self.revoke(&id)),
            Request::Pair { secs } => {
                self.pairing_until = (secs > 0).then(|| now().saturating_add(secs));
                eprintln!("construct hub: pairing window {}", if secs > 0 { "open" } else { "closed" });
                Response::status(self.status())
            }
        }
    }

    /// The live facts admission needs for `id`.
    pub(crate) fn live(&self, id: &str) -> Live {
        Live {
            pending: self.online.values().filter(|c| c.waiting.is_some()).count(),
            pending_key: self.online.get(id).and_then(|c| c.waiting.as_ref()).map(|w| w.key),
            pairing: self.pairing_until.is_some_and(|u| u > now()),
        }
    }

    fn invite(&mut self, id: &str, ttl_secs: u64, url: Option<String>) -> Result<InviteReply> {
        let now = now();
        let (token, inv) = invite::mint(ttl_secs, now)?;
        let expires = inv.expires;
        self.registry.invite(id, inv)?;
        self.save();
        eprintln!("construct hub: invited {id}");
        Ok(invite::reply(id, token, expires, url.unwrap_or_else(|| self.listen.clone()), self.key.clone()))
    }

    fn approve(&mut self, id: &str) -> Result<()> {
        let conn =
            self.online.get_mut(id).filter(|c| c.waiting.is_some()).context(format!("no join is waiting for {id}"))?;
        let waiting = conn.waiting.take().context("checked above")?;
        let _ = conn.tx.send(Verdict::Approved);
        self.registry.pin(id, waiting.key, now());
        self.save();
        Ok(())
    }

    fn deny(&mut self, id: &str) -> Result<()> {
        let conn =
            self.online.get_mut(id).filter(|c| c.waiting.is_some()).context(format!("no join is waiting for {id}"))?;
        conn.waiting = None;
        let _ = conn.tx.send(Verdict::Denied("denied at the hub".into()));
        Ok(())
    }

    fn revoke(&mut self, id: &str) -> Result<()> {
        let offered = self.online.get(id).and_then(|c| c.waiting.as_ref()).map(|w| w.key);
        if self.registry.get(id).is_none() && offered.is_none() {
            bail!("no peer {id}");
        }
        self.registry.revoke(id, offered)?;
        self.save();
        if let Some(conn) = self.online.get_mut(id) {
            conn.waiting = None;
            let _ = conn.tx.send(Verdict::Revoked("revoked at the hub".into()));
        }
        Ok(())
    }
}
