#!/bin/bash
set -euo pipefail
curl --fail --silent --max-time 3 http://127.0.0.1:3001/health >/dev/null
curl --fail --silent --max-time 3 http://127.0.0.1:11434/api/tags >/dev/null
