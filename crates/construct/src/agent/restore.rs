//! Build fresh runtime state around the selected journal. Saved context and
//! accounting survive; permissions, processes and cancellation handles do not.

use super::*;

impl Agent {
    pub fn new(cfg: Config, paths: Paths, cwd: std::path::PathBuf, resume: Option<String>) -> Result<Self> {
        Self::build(cfg, paths, cwd, resume, None, None)
    }

    /// An explicit launch or recovery model choice wins over saved policy.
    pub fn new_with_model(
        cfg: Config,
        paths: Paths,
        cwd: std::path::PathBuf,
        resume: Option<String>,
        model_override: Option<&str>,
    ) -> Result<Self> {
        Self::build(cfg, paths, cwd, resume, None, model_override)
    }

    /// The session of a delegated child: its own prompt, and a head row that
    /// names who delegated.
    pub fn child(cfg: Config, paths: Paths, cwd: std::path::PathBuf, origin: Origin) -> Result<Self> {
        Self::build(cfg, paths, cwd, None, Some(origin), None)
    }

    fn build(
        mut cfg: Config,
        paths: Paths,
        cwd: std::path::PathBuf,
        resume: Option<String>,
        origin: Option<Origin>,
        model_override: Option<&str>,
    ) -> Result<Self> {
        let file = journal::path(&paths, &cwd);
        if let Some(choice) = model_override {
            cfg.select_model(choice)?;
        }
        if let Some(id) = &resume {
            // Validate before credentials, side effects or recovery repairs.
            let local = file.exists() && journal::Journal::read_only(&file)?.last_seq(id)? > 0;
            let source = if local { file.clone() } else { journal::find(&paths, id)?.0 };
            let entries = journal::Journal::read_only(&source)?.strict(id)?;
            journal::validate_directory(&entries, &cwd)?;
            crate::context::check_resume(
                crate::context::strategy_of(entries.iter().map(|e| &e.event)),
                &cfg.context.strategy,
            )?;
            if model_override.is_none()
                && let Some((model, provider)) = journal::saved_model(&entries)
            {
                let selector = match provider {
                    Some(p) => {
                        anyhow::ensure!(
                            cfg.providers.contains_key(&p),
                            "saved model provider {p:?} is unavailable; choose a model with /model or --model"
                        );
                        // Keep selectors that still resolve to the saved provider.
                        // Qualifying a bare preset router can change its wire ID.
                        if cfg.resolve(&model).is_ok_and(|r| r.provider.as_deref() == Some(p.as_str()))
                            || model.starts_with(&format!("{p}/"))
                        {
                            model
                        } else {
                            let qualified = format!("{p}/{model}");
                            anyhow::ensure!(
                                cfg.wire_id(&qualified) == model,
                                "saved model {model:?} cannot be resolved unambiguously on {p}; choose a model with /model or --model"
                            );
                            qualified
                        }
                    }
                    None if cfg.resolve(&model)?.provider.is_some() => format!("default/{model}"),
                    None => model,
                };
                cfg.select_model(&selector).map_err(|e| {
                    anyhow::anyhow!(
                        "saved model {selector:?} is unavailable: {e:#}; choose a model with /model or --model"
                    )
                })?;
            }
        }
        let ep = cfg.endpoint();
        let key = if origin.is_some() { ep.env_key() } else { ep.key() };
        let client = llm::Client::new(&ep, ep.require(key)?)?;
        let memory = crate::memory::Store::new(&paths, &cfg.memory).with_project(&paths, &cwd);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mcp = if cfg.mcp.is_empty() {
            None
        } else {
            Some(Arc::new(crate::mcp::Manager::new(cfg.mcp.clone(), &cfg.sandbox, &cwd, &paths)))
        };
        let is_child = origin.is_some();
        // A delegated child shares the task but not the boundary tools or the
        // notification capability: the parent's time-based note already covers
        // blocks its delegates cause.
        if is_child {
            cfg.deck = None;
        }
        let catalog =
            Arc::new(Catalog::build_with(&cfg.tools, mcp.clone(), cfg.deck.is_some(), cfg.github_gateway.is_some())?);
        if !cfg.agents.is_empty() {
            catalog.add(Arc::new(crate::tools::delegate::Delegate::new(&cfg, &catalog.table(), &cwd)?));
        }
        let manifest = Manifest::new(&cfg, &catalog);
        let cwd_s = cwd.display().to_string();
        let mut unsent = vec![];
        let resume_id = resume.clone();
        let (writer, mut messages) = match resume {
            Some(id) => journal::Writer::resume(&file, &id)?,
            None => {
                let (w, head) = journal::Writer::create(&file, |id| Event::Head {
                    id,
                    harness: "construct".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    cwd: cwd_s.clone(),
                    model: cfg.model.clone(),
                    manifest: manifest.clone(),
                    parent_session: origin.as_ref().map(|o| o.parent_session.clone()),
                    parent_call_id: origin.as_ref().map(|o| o.parent_call_id.clone()),
                })?;
                unsent.push(head);
                (w, vec![])
            }
        };
        let (system, sandbox) = match origin {
            // The child's boundary was decided by its parent; `parent` is the
            // pid the helper watches, and that is this process now.
            Some(o) => (o.system, crate::sandbox::Policy { parent: std::process::id(), ..o.policy }),
            None => (
                crate::prompt::build(&cwd, &memory, &cfg.learn.strategy, cfg.tools.search_tool()),
                crate::sandbox::Policy::new(&cfg.sandbox, &cwd, &paths),
            ),
        };
        let system_recorded = messages.first().is_some_and(|m| m.role == "system");
        match messages.first() {
            Some(m) if m.role == "system" => messages[0] = Message::system(system),
            _ => messages.insert(0, Message::system(system)),
        }
        let context = crate::context::build(&cfg.context.strategy, messages)?;
        let jobs = crate::jobs::Jobs::new(&cfg.jobs);
        if let Some(id) = &resume_id {
            jobs.resume(&journal::Journal::open(&file)?.since(id, 0)?.into_iter().map(|e| e.event).collect::<Vec<_>>());
        }
        // A delegated child shares the task but not the boundary tools: the
        // parent's time-based note already covers blocks its delegates cause.
        let boundary =
            cfg.deck.as_ref().map(|deck| Arc::new(crate::tools::boundary::Boundary::new(&deck.boundary_url)));
        let ctx = Arc::new(
            crate::tools::Ctx::new(cwd, paths.clone(), memory, cancel_rx, mcp, sandbox)
                .with_jobs(jobs)
                .with_boundary(boundary)
                .with_github(cfg.github_gateway.clone()),
        );
        let entries = writer.entries()?;
        let totals = journal::Totals::session(&journal::Journal::read_only(&file)?, &entries)?;
        let mut groups = BTreeSet::new();
        groups.insert(Group::Core);
        if ctx.boundary.is_some() {
            groups.insert(Group::Boundary);
        }
        for e in &entries {
            match &e.event {
                Event::Route { groups: saved, .. } => groups.extend(saved.iter().filter_map(|g| Group::parse(g))),
                Event::Request { tools, .. } => groups.extend(tools.iter().filter_map(|name| catalog.group_of(name))),
                _ => {}
            }
        }
        groups.retain(|g| !matches!(g, Group::Mcp(name) if !cfg.mcp.contains_key(name)));
        let mut agent = Self {
            cfg,
            paths,
            client,
            ctx,
            catalog,
            manifest,
            context,
            groups,
            journal: Arc::new(writer),
            unsent,
            usage: totals.usage,
            last_prompt_tokens: totals.last_prompt,
            window_seen: None,
            bytes_per_token: 4.0,
            turns_since_memory: 0,
            tool_calls_since_skill: 0,
            user_turns: journal::prompts(&entries).len() as u32,
            tool_calls: entries.iter().filter(|e| matches!(e.event, Event::ToolStart { .. })).count() as u32,
            cancel_tx,
            notes: Coalescer::default(),
        };
        if !system_recorded {
            let ev = Event::for_message(agent.messages()[0].clone());
            agent.journal.record(&ev);
            agent.unsent.push(ev);
        }
        if let Some(entry) =
            entries.iter().rev().find(|e| matches!(e.event, Event::Recovered { .. } | Event::TurnEnd { .. }))
            && matches!(entry.event, Event::Recovered { .. })
        {
            agent.unsent.push(entry.event.clone());
        }
        if resume_id.is_some() && model_override.is_some() {
            agent.journal.record(&Event::Model {
                model: agent.cfg.model.clone(),
                provider: agent.cfg.provider_name(),
                context_window: agent.cfg.context_window,
                max_tokens: agent.cfg.max_tokens,
            });
        }
        if let Some(message) = agent.journal.take_failure() {
            agent.unsent.push(Event::Error { message });
        }
        Ok(agent)
    }
}
