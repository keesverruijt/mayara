### Range control

| Control | Unit  |
|---------|-------|
| range   | meter |

The range of a radar describes the maximum distance from the radar antenna that the beam is picking up targets from. At API level it is always in meters.

Some radars support both metric and nautical miles, in which case there is also a control (range_units). Once a type of unit has been selected, the server will attempt to stick to that type of unit when choosing a new range. 

To change range the client can either select a range from the allowed set of ranges described in the radar capabilities, or it can ask for a zoom in or out operation.

The API allows you to ask for _any_ metric range, but this will then be rounded up to the next applicable range. For instance, if you are using a metric range unit, asking for 3000 m will result in the radar returning a range of 3000 m. Asking for 3001 will jump to the next range metric range, for example 4000 m.

