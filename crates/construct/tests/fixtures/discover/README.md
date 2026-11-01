# Discovery fixtures

Response bodies the `llm::discover` parsers are pinned to (ADR 0027, decision 6).

- `llamacpp-models.json`, `llamacpp-props.json`: recorded on 2026-10-31 from a
  llama.cpp server behind a model router (`/v1/models`, three of its entries;
  `/props` with the model loaded, trimmed to the fields read plus a few for
  context, and without the chat template and model path).
- `vllm-models.json`, `ollama-tags.json`, `ollama-show.json`: written by hand
  from the servers' documented response shapes (vLLM's OpenAI-compatible
  `/v1/models`; Ollama's `/api/tags` and `/api/show`). Replace each with a
  recording when one of those servers is available.
