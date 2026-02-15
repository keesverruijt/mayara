#!/bin/bash

set -euo pipefail
set -x

HOST=${1:-10.56.0.1}

BASE_URL="http://${HOST}:6502"
V3="${BASE_URL}/v3/api"

curl -s "${V3}/radars" | jq 
radars=($(curl -s "${V3}/radars" | jq -r '.radars | keys[]'))
echo "Radars: ${radars}"

for radar in ${radars}
do
  curl -s "${V3}/radars" | jq -r ".radars.${radar} | keys[]"
  streamUrl=$(curl -s "${V3}/radars" | jq -r ".radars.${radar}.streamUrl")"?subscribe=none"
  echo "Connecting to $streamUrl"
  websocat "${streamUrl}"
done



