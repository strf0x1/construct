//! Loading a disk session uses normal recovery; attaching an existing live
//! session reads only actor-owned state, without the agent lock or recovery.

use super::Server;
use crate::acp::cwd;
use crate::acp::map::{LEGACY_META_KEY, META_KEY};
use crate::acp::{replay, rpc};
use crate::agent::Agent;
use serde_json::{Value, json};

/// The load snapshot under both the reverse-DNS key and the bare one, so a
/// released Deck still finds it (issue #171).
fn snapshot(turn: Value) -> Value {
    json!({ "_meta": {
        META_KEY: { "turn": turn.clone() },
        LEGACY_META_KEY: { "turn": turn },
    } })
}

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
            let (updates, turn) = s.attach(sid, &cwd, self.attachment, replay)?;
            for update in updates {
                let _ =
                    self.out.send(rpc::notification("session/update", json!({ "sessionId": sid, "update": update })));
            }
            self.questions.send(sid, &self.out);
            self.turn_notice(sid);
            return Ok(snapshot(turn));
        }
        if crate::journal::find(&self.paths, sid).is_err() {
            return Err((rpc::NOT_FOUND, format!("no session {sid}")));
        }
        // Ownership, recovery, the directory check and the saved model are
        // the resume path's, so `--resume` and ACP cannot disagree. The
        // client's `mcpServers` are ignored, as in `session/new`: construct's
        // servers come from config, and their credentials never reach a log.
        let agent = Agent::new_with_tools(
            self.cfg.clone(),
            self.paths.clone(),
            cwd,
            Some(sid.to_string()),
            None,
            &self.sources,
        )
        .map_err(|e| (rpc::INVALID_PARAMS, format!("{e:#}")))?;
        if replay {
            let entries = agent.entries().map_err(|e| (rpc::INTERNAL, format!("{e:#}")))?;
            let catalog = agent.catalog();
            for update in replay::history(&entries, &|name| catalog.kind_of(name)) {
                let line = rpc::notification("session/update", json!({ "sessionId": sid, "update": update }));
                let _ = self.out.send(line);
            }
        }
        self.register(agent).map_err(|e| (rpc::INTERNAL, format!("{e:#}")))?;
        if replay {
            let turn = self.sessions[sid].turn_value(sid);
            self.turn_notice(sid);
            Ok(snapshot(turn))
        } else {
            Ok(json!({}))
        }
    }
}
