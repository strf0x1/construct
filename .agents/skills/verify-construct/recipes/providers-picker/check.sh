# Only the default provider was sent anything, the switch to `local` is a
# model row naming the provider, and the refused switch to `gpubox` left none.
set -e
n=$(ls "$EVIDENCE"/requests/req-*.json | wc -l)
[ "$n" = 1 ] || { echo "expected 1 request to the default endpoint, got $n"; exit 1; }
[ "$(jq -r .model "$EVIDENCE"/requests/req-*.json)" = "anthropic/claude-haiku-5.5" ] || { echo "wrong model sent"; exit 1; }
rows=$(grep '|model|' "$EVIDENCE/journal.txt" | sed 's/^[^{]*//')
[ "$(echo "$rows" | wc -l)" = 1 ] || { echo "expected one model row, got: $rows"; exit 1; }
[ "$(echo "$rows" | jq -r .provider)" = local ] || { echo "model row lacks provider local: $rows"; exit 1; }
[ "$(echo "$rows" | jq -r .context_window)" = 32768 ] || { echo "model row lacks the entry's 32768: $rows"; exit 1; }
head=$(grep '|head|' "$EVIDENCE/journal.txt" | head -1 | sed 's/^[^{]*//')
[ "$(echo "$head" | jq -r '.manifest.provider // "default"')" = default ] || { echo "head names a provider: $head"; exit 1; }
echo "ok: one request to default; model row local/qwen3-8b provider local 32768; gpubox refused"
