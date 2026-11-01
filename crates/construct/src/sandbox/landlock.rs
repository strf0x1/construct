//! The Linux backend: a Landlock ruleset applied to the helper process, which
//! every descendant then inherits (ADR 0004).

use super::Policy;
use std::path::PathBuf;

/// Apply the Landlock ruleset to this process and everything it will start.
pub(super) fn restrict(policy: &Policy) -> Result<(), String> {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, NetPort, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetStatus, Scope, path_beneath_rules,
    };
    let abi = ABI::V6;
    let ro = AccessFs::from_read(abi);
    let rw = AccessFs::from_all(abi);
    // A grant for a place that doesn't exist (yet) has nothing to restrict,
    // and a rule for it would fail the whole ruleset.
    let live = |v: &[PathBuf]| v.iter().filter(|p| p.exists()).cloned().collect::<Vec<_>>();
    let (system, read, write) = if policy.net_only {
        Default::default()
    } else {
        (live(&policy.system), live(&policy.read), live(&policy.write))
    };
    let base = Ruleset::default().set_compatibility(CompatLevel::BestEffort);
    // A delegated child's process handles no filesystem access at all.
    let status = Ok(base)
        .and_then(|r: Ruleset| if policy.net_only { Ok(r) } else { r.handle_access(rw) })
        // Handling TCP access with no rule allowing a port denies it all.
        .and_then(|r| if policy.no_network { r.handle_access(AccessNet::from_all(abi)) } else { Ok(r) })
        .and_then(|r| r.scope(Scope::Signal))
        .and_then(|r| r.create())
        // A port rule for access the ruleset doesn't handle is an error, so
        // the exceptions exist only when TCP is denied.
        .and_then(|r| {
            let ports = if policy.no_network { policy.connect_ports.as_slice() } else { &[] };
            r.add_rules(ports.iter().map(|p| Ok(NetPort::new(*p, AccessNet::ConnectTcp))))
        })
        .and_then(|r| r.add_rules(path_beneath_rules(&system, ro)))
        .and_then(|r| r.add_rules(path_beneath_rules(&read, ro)))
        .and_then(|r| r.add_rules(path_beneath_rules(&write, rw)))
        .and_then(|r| r.restrict_self())
        .map_err(|e| format!("landlock: {e}"))?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err("Landlock isn't available on this kernel, so commands can't be sandboxed. Set [sandbox] mode = \"off\" in config.toml to run without it.".into());
    }
    Ok(())
}
