#!/bin/bash

set -euo pipefail

HOST=${1:-10.56.0.1}

BASE_URL="http://${HOST}:6502"
V2="${BASE_URL}/signalk/v2/api/vessels/self/radars"

radars=($(curl -s "${V2}" | jq -r '.radars | keys[]'))
echo "Radars: ${radars}"

radar=${radars}
curl -s "${V2}/${radar}/controls/noTransmitSector1"
curl -s --json '{"value":-85,"endValue":-60,"units":"deg","enabled":true}' "${V2}/${radar}/controls/noTransmitSector1"
curl -s "${V2}/${radar}/controls/noTransmitSector1"



