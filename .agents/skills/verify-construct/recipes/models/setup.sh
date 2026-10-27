# OpenRouter's dialect on the loopback mock, which serves /models in
# OpenRouter's shape, with a model whose window is 32768.
cat > "$CHOME/config.toml" <<'TOML'
model = "mock/small"

[provider]
dialect = "openrouter"
TOML
