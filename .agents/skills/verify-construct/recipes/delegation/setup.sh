echo "scout-data" > "$OUTSIDE/f.txt"
cat > "$CHOME/config.toml" <<'TOML'
[agents.scout]
description = "reads files and reports"
prompt = "You are a scout."
tools = ["read"]
TOML
