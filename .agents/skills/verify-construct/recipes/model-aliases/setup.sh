# The default endpoint is the scripted model, with OpenRouter's dialect so
# startup discovery fetches its list. That list has one canonical `mock/small`
# with `aliases: ["compact", "small"]`; no hand-written catalog.
cat > "$CHOME/config.toml" <<'TOML'
model = "mock"
[provider]
dialect = "openrouter"
[router]
mode = "local"
[learn]
enabled = false
TOML
