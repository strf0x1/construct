//! Loading a disk session uses normal recovery; attaching an existing live
//! session reads only actor-owned state, without the agent lock or recovery.

use super::map::{self, META_KEY};
use super::rpc;
use super::server::{Server, cwd};
use crate::agent::Agent;
use serde_json::{Value, json};

impl Server {
    /// `session/load` replays the recorded conversation before it answers;
    /// `session/resume` is for a client that still has it.
    pub(super) fn load(&mut self, id: &Value, params: &Value, replay: bool) {
        let r = self.try_load(params, replay);
        let _ = self.out.send(match r {
            Ok(v) => rpc::ok(id, v),
            Err((rpc::NOT_FOUND, m)) => {
                rpc::err_data(id, rpc::NOT_FOUND, &m, json!({ "sessionId": params["sessionId"] }))
            }
            Err((code, m)) => rpc::err(id, code, &m),
        });
    }

    fn try_load(&mut self, params: &Value, replay: bool) -> Result<Value, (i64, String)> {
        let Some(sid) = params["sessionId"].as_str().filter(|s| !s.is_empty()) else {
            return Err((rpc::INVALID_PARAMS, "sessionId is required".into()));
        };
        let cwd = cwd(params).map_err(|e| (rpc::INVALID_PARAMS, format!("{e:#}")))?;
        if let Some(s) = self.sessions.get_mut(sid) {
            if s.cwd.as_os_str() != cwd.as_os_str() {
                return Err((
                    rpc::INVALID_PARAMS,
                    format!("session {sid} belongs to {}, not {}", s.cwd.display(), cwd.display()),
                ));
            }
            if s.attached == self.attachment {
                return Err((rpc::INVALID_PARAMS, format!("session {sid} is already loaded on this connection")));
            }
            if replay {
                for update in s.replay() {
                    let _ = self
                        .out
                        .send(rpc::notification("session/update", json!({ "sessionId": sid, "update": update })));
                }
            }
            s.attached = self.attachment;
            let turn = s.turn.value(sid);
            self.questions.send(sid, &self.out);
            self.turn_notice(sid);
            return Ok(json!({ "_meta": { META_KEY: { "turn": turn } } }));
        }
        if crate::journal::find(&self.paths, sid).is_err() {
            return Err((rpc::NOT_FOUND, format!("no session {sid}")));
        }
        // Ownership, recovery, the directory check and the saved model are
        // the resume path's, so `--resume` and ACP cannot disagree. The
        // client's `mcpServers` are ignored, as in `session/new`: construct's
        // servers come from config, and their credentials never reach a log.
        let agent = Agent::new(self.cfg.clone(), self.paths.clone(), cwd, Some(sid.to_string()))
            .map_err(|e| (rpc::INVALID_PARAMS, format!("{e:#}")))?;
        if replay {
            let entries = agent.journal.entries().map_err(|e| (rpc::INTERNAL, format!("{e:#}")))?;
            let catalog = agent.catalog.clone();
            for update in map::history(&entries, &|name| catalog.kind_of(name)) {
                let line = rpc::notification("session/update", json!({ "sessionId": sid, "update": update }));
                let _ = self.out.send(line);
            }
        }
        self.register(agent).map_err(|e| (rpc::INTERNAL, format!("{e:#}")))?;
        if replay {
            let turn = self.sessions[sid].turn.value(sid);
            self.turn_notice(sid);
            Ok(json!({ "_meta": { META_KEY: { "turn": turn } } }))
        } else {
            Ok(json!({}))
        }
    }
}
