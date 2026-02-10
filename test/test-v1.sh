#!/bin/bash

set -euo pipefail

HOST=${1:-10.56.0.1}

BASE_URL="http://${HOST}:6502"
V1="${BASE_URL}/v1/api"

radars="$(curl -s "${V1}/radars")"
echo "${radars}"
echo ""
echo "---------"
radarnames=$(echo "${radars}" | jq -r 'keys[]')
echo "Radars: ${radarnames}"

for radar in ${radarnames}
do
  echo "Radar ${radar}:"

  echo "${radars}" | jq -r '."'${radar}'"'
  echo "${radars}" | jq -r ".\"${radar}\".controls"
  controlIds=$(echo "${radars}" | jq -r ".\"${radar}\".controls"' | keys[]')
  for i in ${controlIds}
  do
    echo "------------ control ${i}"
  done
done



