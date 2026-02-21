#!/bin/bash

set -euo pipefail

HOST=${1:-10.56.0.1}

BASE_URL="http://${HOST}:6502"
V2="${BASE_URL}/signalk/v2/api/vessels/self/radars"

for i in "${V2}/resources/openapi.json" "${V2}/interfaces" "${V2}"
do
  echo "------------ ${i}"
  curl -s "${i}"
  echo ""
done

radars=($(curl -s "${V2}" | jq -r '.radars | keys[]'))
echo "Radars: ${radars}"

for radar in ${radars}
do
  curl -s "${V2}/${radar}/capabilities"
  controlIds=$(curl -s "${V2}/${radar}/capabilities" | jq -r ".radars.${radar}.capabilities.controls | keys[]")
  echo "------------- radar ${radar} all controls"
  curl -s "${V2}/${radar}/controls"
  echo ""
  for i in ${controlIds}
  do
    echo "------------ radar ${radar} control ${i}"
    curl -s "${V2}/${radar}/controls/${i}"
    echo ""
  done
done



