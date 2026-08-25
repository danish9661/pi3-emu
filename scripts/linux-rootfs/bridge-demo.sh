#!/bin/sh
# pi3-ctl N4 bridge exerciser for pi3-emu.
#
# Usage:  bridge-demo
#
# Browser -> guest: this script watches an INPUT pin (default 23). In the
# pi3-emu UI, click a "Bridge" button (e.g. 23) to drive that pin from the
# browser; this script prints the live value the guest sees via GPLEV.
#
# Guest -> browser: this script echoes the input state out to an OUTPUT pin
# (default 21), so the UI toolbar readout shows "guest->browser: G21=1/0".
# This gives a full round-trip: click Bridge 23 (high) -> this script writes
# 21 (high) -> UI shows G21=1.
IN=23
OUT=21

GPIO=/sys/class/gpio
if [ ! -d "$GPIO" ]; then
    echo "ERROR: $GPIO not present (kernel lacks CONFIG_GPIO_SYSFS?)" >&2
    echo "       The N4 bridge still works; this demo just can't read pins." >&2
    exit 1
fi

echo "pi3-ctl bridge demo: watching GPIO$IN (input), echoing to GPIO$OUT (output)"
echo "  In the pi3-emu UI, use the Bridge buttons (23/24/25/26/27) to drive the input."
echo "  Watch the toolbar 'guest->browser' readout for G$OUT."
for p in $IN $OUT; do
    if [ ! -d "$GPIO/gpio$p" ]; then
        echo "$p" > "$GPIO/export" 2>/dev/null
        usleep 200000
    fi
done
echo "out" > "$GPIO/gpio$OUT/direction" 2>/dev/null
echo "in"  > "$GPIO/gpio$IN/direction"  2>/dev/null

while true; do
    v=$(cat "$GPIO/gpio$IN/value" 2>/dev/null)
    echo "GPIO$IN (browser->guest) = ${v:-?}"
    echo "${v:-0}" > "$GPIO/gpio$OUT/value" 2>/dev/null
    sleep 1
done
