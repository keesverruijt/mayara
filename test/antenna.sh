#!/bin/bash

set -euo pipefail

HOST=${1:-10.56.0.1}

BASE_URL="http://${HOST}:6502"
V2="${BASE_URL}/signalk/v2/api/vessels/self/radars"

radars=($(curl -s "${V2}" | jq -r '.radars | keys[]'))
echo "Radars: ${radars}"

radar=${radars}
url="${V2}/${radar}/controls/antennaHeight"
curl -s "${url}"
echo ''
curl -s -X PUT --json '{"value":8,"units":"m"}' "${url}"
echo ''
curl -s "${url}"
echo ''



