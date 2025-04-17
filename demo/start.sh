#!/bin/sh
./mayara -i lo -p 3001 --replay --brand navico --targets trails &
signalk-server/bin/signalk-server -c signalk/ &
tcpreplay -q -T select -l 0 -i lo /app/halo_and_0183.pcap
