### TODO.md


A list of things still to do before this code has caught back up with Dirk's vibe coded version:

1. Implement proper Signal K GET and PUT requests that allow all data to be retrieved by a HTTP client. Most are there but some distance to go.
   1.1 - Return values in SI units, including in the definition of controls
   1.2 - Accept values in any unit that can be converted and apply it correctly.

2. Implement Signal K delta updates, in the official way with subscriptions etc.
   After step 1 this should be do-able.

3. Check and finagle the Furuno code back into operation. It should work, but hasn't been tested.

4. Re-implement the radar recording and playback.

5. Re-implement the debugger. Or a better one?


After that:

* Target acquisition (M)ARPA using internal matching code
* Guard zones (internal)
* Timed Transmit
* Garmin support (on hold until developer shows up)
