# A hand-written catalog: three models with different windows, one on its own
# endpoint, one not in the file at all (the default model's entry is the first).
cat > "$CHOME/config.toml" <<'TOML'
model = "anthropic/claude-haiku-5.5"
context_window = 131072
[router]
mode = "local"
[learn]
enabled = false
TOML
cat > "$CHOME/models.toml" <<'TOML'
[models."anthropic/claude-haiku-5.5"]
label = "Haiku 5.5 — fast recon"
description = "Cheap and quick. Good for searching a codebase."
tags = ["fast", "tools"]
context_window = 200000
max_tokens = 16000
pricing = { prompt = 1.0, completion = 5.0 }

[models."anthropic/claude-opus-5.5"]
label = "Opus 5.5 — planning"
description = "Slow and careful. Use it to plan, not to grep."
tags = ["reasoning", "vision"]
context_window = 1000000
max_tokens = 32000
pricing = { prompt = 5.0, completion = 25.0 }

[models."mini/tiny-8k"]
label = "Tiny 8k — compaction test"
context_window = 8192
max_tokens = 1024

[models."local/qwen3"]
label = "Qwen3 on the box"
tags = ["local"]
context_window = 65536
base_url = "http://127.0.0.1:1/v1"
api_key_secret = ""
pricing = { prompt = 0.0, completion = 0.0 }
TOML
