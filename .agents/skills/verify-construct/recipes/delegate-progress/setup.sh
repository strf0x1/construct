cat > "$CHOME/config.toml" <<'TOML'
[agents.scout]
description = "waits and reports"
prompt = "You are a scout."
tools = ["shell"]
TOML
