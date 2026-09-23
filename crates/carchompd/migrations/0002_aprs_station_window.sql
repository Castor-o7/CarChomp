-- aprs_station used to find the latest report of every name ever heard and
-- only then drop the old ones, so each map refresh scanned the whole packet
-- log, which a busy RF channel grows by 10^5 rows a day. Bound it to the
-- last day first, so the time index narrows the scan before DISTINCT ON.
-- A station silent for longer than that is off the map whatever the caller
-- asks for.
CREATE OR REPLACE VIEW aprs_station AS
SELECT * FROM (
    SELECT DISTINCT ON (name) * FROM aprs_packet
    WHERE geom IS NOT NULL AND time > now() - interval '1 day'
    ORDER BY name, time DESC
) latest
WHERE alive;
