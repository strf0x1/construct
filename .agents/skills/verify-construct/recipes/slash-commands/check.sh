# Slash commands are local: none of them reached the model.
set -e
[ ! -s "$EVIDENCE/requests.jsonl" ] || { echo "a slash command sent a model request"; exit 1; }
echo "ok: no model requests"
