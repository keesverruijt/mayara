#!/bin/bash

set -euo pipefail

HOST=${1:-10.56.0.1}

BASE_URL="http://${HOST}:6502"
V1="${BASE_URL}/v1/api"

radars="$(curl -s "${V1}/radars")"
echo "${radars}"
echo ""
echo "---------"
radars=$(echo "${radars}" | jq -r 'keys[]')
echo "Radars: ${radars}"

for radar in ${radars}
do
  echo "Radar ${radar}:"

  controlIds=$(echo "${radars}" | jq -r ".\"${radar}\".controls"' | keys[]')
  for i in ${controlIds}
  do
    echo "------------ control ${i}"
  done
done



