### TODO.md


Gui changes to do:

- fix padding/border in read only values
- check why hours are rounded to a half hour
- fix double enable checkboxes in NoTransmitSector
- check doppler packets sent when no chartplotter present
- move radar controls for trails and targets into separate groups

A list of things still to do before this code has caught back up with Dirk's vibe coded version:

1. Check and finagle the Furuno code back into operation. It should work, but hasn't been tested.
   (-> Dirk)

2. Re-implement the radar recording and playback. Consider basing this on the Signal K playback
   method.

3. Re-implement the debugger. Or a better one?
   Kees: I did not really see the point of it, TBH

Also:

* Tests, tests!
* Target acquisition (M)ARPA using internal matching code
* Guard zones (internal)
* Timed Transmit
* Garmin support (on hold until developer shows up)
