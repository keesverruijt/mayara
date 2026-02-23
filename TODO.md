### TODO.md

GUI parts that do not work yet before we can call this a full replacement of `radar_pi`:

* Guard zones can be created and edited but they are not actively supported yet.
* (M)ARPA target tracking is not actively supported yet.
* EBL/VRM handling

Bugs:

* Rotation of the PPI window doesn't work, image is always HeadsUp whereas heading check marks
  are north up.

Server side:

* check doppler packets sent when no chartplotter present and disallow doppler status when
  no heading is on radar spokes.
* Check and finagle the Furuno code back into operation. It should work, but hasn't been tested.
   (-> Dirk)
* Re-implement the radar recording and playback. 
* Re-implement the debugger. Or a better one?  Kees: I did not really see the point of it, TBH

Also:

* Tests, tests!
* Timed Transmit
* Garmin support (on hold until developer shows up)
