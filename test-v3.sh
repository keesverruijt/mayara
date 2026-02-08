#!/bin/bash

set -euo pipefail

HOST=${1:-10.56.0.1}

BASE_URL="http://${HOST}:6502"
V3="${BASE_URL}/v3/api"

for i in "${V3}/openapi.json" "${V3}/interfaces" "${V3}/radars" "${V3}/radars/1/capabilities"
do
  echo "------------ ${i}"
  curl -s "${i}"
  echo ""
done

controlIds=$(curl -s "${V3}/radars/1/capabilities" | jq -r '.controls | keys[]')
for i in ${controlIds}
do
  echo "------------ control ${i}"
  curl -s "${V3}/radars/1/controls/${i}"
  echo ""
done



