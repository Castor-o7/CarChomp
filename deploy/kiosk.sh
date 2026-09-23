#!/bin/sh
# The kiosk browser, started from the desktop user's labwc autostart
# (deploy/install.sh --kiosk).
# A car computer stops by power cut: mark the last session as a clean exit so
# Chromium does not offer to restore pages over the map on every start.
p=$HOME/.config/chromium/Default/Preferences
[ ! -f "$p" ] || sed -i -e 's/"exited_cleanly":false/"exited_cleanly":true/' -e 's/"exit_type":"[^"]*"/"exit_type":"Normal"/' "$p"
# Chromium's error page would never retry by itself: wait for carchompd.
until curl -sf http://localhost/api/health >/dev/null; do sleep 1; done
exec chromium http://localhost --kiosk --password-store=basic --noerrdialogs --disable-infobars --no-first-run \
    --disable-session-crashed-bubble --enable-features=OverlayScrollbar
