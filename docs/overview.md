### Mayara overview

This document is meant to explain what Mayara does and why it does things in a particular manner. MA(rine) YA(cht) RA(dar) opens up the capabilities of the major yacht radars. It can always be used together with the brand multi-function displays (MFDs). It can also be used
without such a MFD.

# Role

Mayara serves as a uniform layer that abstracts radars in such a way that the client can (almost) forget about the differences between radars. It certainly doesn't need to be rewritten from scratch for every radar! Unfortunately, there are some differences between how radars work that we can't hide (yet), like how dual range is handled. A complete implementation that supports dual range for all radars will therefore have to support two ways of handling this. Once we learn more, we may be able to hide these differences behind a uniform single method of dealing with this.

# Possible clients

The Mayara server can have multiple clients, and these clients can be of two main types:
1. End user applications that emulate a PPI (Position Plot Indicator), the name of a traditional radar display, or chart applications that show a radar overlay layer.
2. Intermediate applications that either automate certain aspects or use the radar image to perform actions. This could be an autonomous yacht that self-steers around obstacles, or an application that sends the radar image to a central location, etc. 

# Technologies used

The technologies used on the client side are in Signal K format, e.g. JSON over some transport mechanism like HTTP, WS or TCP. It is our goal to support all of these, making it easy for all applications to use Mayara.

# WASM 

In the course of producing mayara we experimented with running the entire Rust code base in WASM inside Node.js, which would make integration with Node based applications super easy. Unfortunately, the performance was sub-par. For now, these developments have been laid aside awaiting further developments in the WASM technology stack.

# Wireless?

Modern radars all "talk" to the MFD (and Mayara) using (mostly) WIRED Ethernet. There are some radars that are wireless only (Furuno DRS4W) or support wireless as well (Raymarine Quantum). At the moment, neither DRS4W nor Quantum is supported via wireless. The reason for not supporting DRS4W is that nobody has stepped forward yet allowing us to observe it in operation. For Quantum, the main difficulty is that the radar requires a secret handshake
without which it will not function longer than a few minutes. This secret handshake has not been reverse engineered yet, so until someone writes the missing bits it won't work.

Most other radars use multicast to send radar spokes to the clients on the network. Although this works really well, without any overhead, on wired ethernet it does NOT function well with WiFi. With multicast over WiFi the packets need to be sent at the lowest possible data rate in order for the packets to arrive. This does not function well with the ~ 1 Megabyte per second that a radar produces. This results in missing spokes. Even with 5 GHz networks.

In fact, this is one of the original use cases for mayara: run the mayara server on a small computer with access to the radar over wired Ethernet, and access the server via wifi from a phone, tablet or PC.
