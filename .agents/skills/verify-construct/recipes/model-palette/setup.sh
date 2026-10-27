# OpenRouter's dialect on the loopback mock, which serves /models in
# OpenRouter's shape. The session starts on `mock`, so the palette has to
# be what moves it to mock/small.
cat > "$CHOME/config.toml" <<'TOML'
model = "mock"

[provider]
dialect = "openrouter"
TOML
