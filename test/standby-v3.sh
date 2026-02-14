#!/bin/bash

set -euo pipefail

HOST=${1:-10.56.0.1}

BASE_URL="http://${HOST}:6502"
V3="${BASE_URL}/v3/api"

radars=$(curl -s "${V3}/radars" | jq -r '.radars | keys[]')

for radar in ${radars}
do
  echo "Standby radar ${radar}"
  curl -s --json '{"value":"standby"}' "${V3}/radars/${radar}/controls/power"
  echo ""
done



