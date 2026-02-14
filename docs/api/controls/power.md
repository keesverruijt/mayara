### Power control

| Control | Type  |
|---------|-------|
| power   | enum  |

The power status control of a radar describes whether it is powered off, in standby mode or actively transmitting. Additionaly magnetron radars have a phase where before being able to transmit the magnetron is being heated up. Modern IC only frontend radars do not need this and only have a short "spinning up" phase. Both of these are called __preparing__ in Mayara.

Most radars only allow the user to select the "standby" and "transmitting" modes, the others are for reporting only. Some radars allow selecting "off", but then you generally need a power cycle to restart the radar.

