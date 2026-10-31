# Two named providers that both list qwen3-8b, besides the top-level endpoint
# (the scripted model, via CONSTRUCT_BASE_URL). `local` is loopback, so it is
# keyless; `gpubox` takes GPUBOX_API_KEY, which nothing sets. Neither is
# reachable: the recipe switches to them, it never sends to them.
cat > "$CHOME/config.toml" <<'TOML'
model = "anthropic/claude-haiku-5.5"
context_window = 131072
[router]
mode = "local"
[learn]
enabled = false

[providers.local]
base_url = "http://127.0.0.1:1/v1"
context_window = 65536
[providers.local.models."qwen3-8b"]
label = "Qwen3 8B on the laptop"
context_window = 32768

[providers.gpubox]
base_url = "http://10.255.255.1:8000/v1"
[providers.gpubox.models."qwen3-8b"]
label = "Qwen3 8B on the GPU box"
TOML
