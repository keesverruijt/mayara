#!/bin/bash

set -euo pipefail

HOST=${1:-10.56.0.1}

BASE_URL="http://${HOST}:6502"
V3="${BASE_URL}/v3/api"

for i in "${V3}/resources/openapi.json" "${V3}/interfaces" "${V3}/radars" "${V3}/radars/1/capabilities"
do
  echo "------------ ${i}"
  curl -s "${i}"
  echo ""
done

radars=$(curl -s "${V3}/radars" | jq -r 'keys[]')
echo "Radars: ${radars}"

for radar in ${radars}
do
  controlIds=$(curl -s "${V3}/radars/${radar}/capabilities" | jq -r '.controls | keys[]')
  echo "------------- radar ${radar} all controls"
  curl -s "${V3}/radars/${radar}/controls"
  echo ""
  for i in ${controlIds}
  do
    echo "------------ radar ${radar} control ${i}"
    curl -s "${V3}/radars/${radar}/controls/${i}"
    echo ""
  done
done



