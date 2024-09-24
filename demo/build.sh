#!/bin/sh

set -euo pipefail

docker buildx build --no-cache -t keesverruijt/mayara:latest .

echo "Now run the image locally with:

docker run --name mayara-demo -p 3000-3001:3000-3001 keesverruijt/mayara:latest
"

