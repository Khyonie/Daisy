#!/bin/bash
set -Eeuo pipefail

children=()
cleanup() {
    trap - EXIT TERM INT
    if ((${#children[@]})); then
        kill -TERM "${children[@]}" 2>/dev/null || true
        wait "${children[@]}" 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'exit 0' TERM INT

# The bundled services communicate over loopback, never a host-installed Ollama.
export OLLAMA_HOST=127.0.0.1:11434
export DOCUMENTLLM_OLLAMA_URL=http://127.0.0.1:11434
model=${DOCUMENTLLM_MODEL:-gemma4:e4b}

mkdir -p /data/upload "$OLLAMA_MODELS" "$DOCUMENTLLM_EMBEDDING_CACHE_PATH"
echo "Starting bundled Ollama..."
ollama serve &
ollama_pid=$!
children+=("$ollama_pid")

ready=false
for ((attempt = 0; attempt < 60; attempt++)); do
    if curl --fail --silent --max-time 2 "$DOCUMENTLLM_OLLAMA_URL/api/tags" >/dev/null; then
        ready=true
        break
    fi
    if ! kill -0 "$ollama_pid" 2>/dev/null; then
        echo "Ollama exited before becoming ready." >&2
        exit 1
    fi
    sleep 1
done
if [[ "$ready" != true ]]; then
    echo "Ollama did not become ready within the startup timeout." >&2
    exit 1
fi

if ollama show "$model" >/dev/null 2>&1; then
    echo "Using cached model: $model"
else
    echo "Downloading $model. First startup may take several minutes..."
    ollama pull "$model" &
    bootstrap_pid=$!
    children+=("$bootstrap_pid")
    wait "$bootstrap_pid"
    children=("$ollama_pid")
fi

echo "Preparing the document embedding model..."
documentllm-server --prepare-embeddings &
bootstrap_pid=$!
children+=("$bootstrap_pid")
wait "$bootstrap_pid"
children=("$ollama_pid")

echo "Starting DocumentLLM on port 3001..."
"$@" &
app_pid=$!
children+=("$app_pid")

# Either service exiting must stop the container, so restart policies can recover it.
status=0
wait -n "$ollama_pid" "$app_pid" || status=$?
echo "A bundled service exited; shutting down DocumentLLM." >&2
if ((status == 0)); then
    status=1
fi
exit "$status"
