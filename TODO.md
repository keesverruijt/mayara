### TODO.md

Working:

* Start, logging
* Detect BR24, 3G, 4G and HALO radars
* Provide webserver for static and dynamic pages
* Serve navico radar data
* Control navico radar (tested with 4G and HALO)
* Detect Furuno radar (tested with DRS4D-NXT)

Work in Progress:

* Read data from Furuno
* Detect Garmin xHD (but not yet know if different from HD)

TODO:

* Everything else


# Example output:

Removing a network card:

    [2024-08-18T10:26:25Z WARN  mayara::locator] Interface 'en7' became inactive or lost its IPv4 address

Enabling it again:

    [2024-08-18T10:26:55Z INFO  mayara::locator] Interface 'en7' became active or gained an IPv4 address, now listening on IP address 169.254.91.182/255.255.0.0 for radars
    [2024-08-18T10:26:55Z INFO  mayara::radar] Located a new radar: Radar 1 locator Navico 3G, 4G, HALO brand Navico A [1403302452] at 169.254.24.199 via 169.254.91.182 data 236.6.7.8:6678 report 236.6.7.9:6679 send 236.6.7.10:6680
    [2024-08-18T10:26:55Z INFO  mayara::radar] Located a new radar: Radar 2 locator Navico 3G, 4G, HALO brand Navico B [1403302452] at 169.254.24.199 via 169.254.91.182 data 236.6.7.13:6657 report 236.6.7.15:6659 send 236.6.7.14:6658


