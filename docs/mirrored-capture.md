# Capturing a mirrored game connection

The ordinary capture mode reads this PC's traffic without promiscuous mode and
waits for a local AION2 window. A dedicated monitoring PC can instead read a
copy of another PC's game traffic supplied by a switch SPAN/mirror port.
This setup supports the monitoring-PC use case described in
[issue #84](https://github.com/taengu/A2Tools-DPS-Meter/issues/84).

1. Configure the switch to mirror the game PC's connection to a dedicated
   monitoring adapter. The meter does not configure the switch or forward
   packets.
2. On the monitoring PC, install the platform's capture library (Npcap on
   Windows, libpcap on Linux), with the normal capture permissions.
3. Open **Settings → Connection** and turn on **Capture traffic mirrored
   from another PC**. The adapter list now includes interfaces without an IP
   address. Select the dedicated mirror adapter. Its OS device ID is shown
   alongside its description so identically named adapters remain distinct.
4. Restart the meter. It opens only that selected adapter in promiscuous mode.
   A missing or unavailable selection leaves capture stopped; it never falls
   back to opening every adapter promiscuously.
5. The game packets can identify your character as usual. If necessary, use the
   existing character name and actor-ID controls in Settings. No placeholder
   game process or window is needed on this monitoring PC.

The monitoring adapter may remain without an IP address. Remote-game capture
also ignores the meter's local-game focus and closed-window hiding rules. Party and
out-of-combat hiding still use the received game data. Automatic and mirrored
adapter choices are stored separately, so changing modes preserves the ordinary
adapter preference. Restart again after changing capture mode.

Plugins can still receive captured game data. A plugin's optional **hide while
AION2 is closed** setting continues to refer to a local game window. Disable that
option for plugins you want visible on the monitoring PC; mirrored capture does
not change the plugin game-window API.

This is a capture mode, not a guarantee of an offline application: an ordinary
online build retains its account, sharing and update features. Build with the
documented offline Cargo features when those features must be absent, and keep
the monitoring environment isolated as appropriate for the setup.

Synthetic tests cover default-off behavior, window/IP independence in mirrored
mode, explicit adapter selection, duplicate descriptions, and asynchronous
adapter-list changes. Live switch/SPAN and driver behavior still needs validation
on a real monitoring setup.
