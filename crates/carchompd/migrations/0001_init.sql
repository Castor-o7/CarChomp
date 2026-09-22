CREATE EXTENSION IF NOT EXISTS postgis;

-- Anything that produces observations: a GPS receiver, an SDR, a camera.
CREATE TABLE source (
    id     smallint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind   text  NOT NULL,
    name   text  NOT NULL,
    config jsonb NOT NULL DEFAULT '{}',
    UNIQUE (kind, name)
);

CREATE TABLE track (
    id      bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name    text,
    started timestamptz NOT NULL,
    ended   timestamptz,
    visible boolean NOT NULL DEFAULT true
);

-- Our own position. Only fixes selected by smart beaconing are stored.
CREATE TABLE fix (
    time      timestamptz NOT NULL,
    source_id smallint NOT NULL REFERENCES source,
    track_id  bigint   NOT NULL REFERENCES track ON DELETE CASCADE,
    geom      geometry(Point, 4326) NOT NULL,
    alt       real,  -- metres above the WGS84 ellipsoid
    speed     real,  -- m/s
    course    real,  -- degrees true
    h_err     real,  -- metres; lets sources of differing quality coexist
    PRIMARY KEY (track_id, time, source_id)
);
CREATE INDEX fix_time ON fix USING brin (time);

-- A finished track cut into short pieces. One long LineString has a bounding
-- box the size of the whole drive, which defeats a spatial index; many short
-- ones make "have I been here?" an index lookup.
CREATE TABLE track_segment (
    track_id bigint NOT NULL REFERENCES track ON DELETE CASCADE,
    geom     geography(LineString, 4326) NOT NULL
);
CREATE INDEX track_segment_geom  ON track_segment USING gist (geom);
-- Planar twin of the index above, for map viewport (bounding box) queries.
CREATE INDEX track_segment_bbox  ON track_segment USING gist ((geom::geometry));
CREATE INDEX track_segment_track ON track_segment (track_id);

-- Every APRS packet heard. `raw` is the whole packet in TNC2 form, so
-- packets can be re-parsed as the parser learns more formats.
CREATE TABLE aprs_packet (
    time      timestamptz NOT NULL DEFAULT now(),
    source_id smallint NOT NULL REFERENCES source,
    callsign  text    NOT NULL,  -- who sent it
    -- Heard without a digipeater, so the sender is within radio range of us.
    -- The raw material for estimating our own position from RF alone.
    direct    boolean NOT NULL,
    kind      text    NOT NULL,  -- position | object | weather | message | status | other
    name      text    NOT NULL,  -- what it is about: an object's name, else the callsign
    alive     boolean NOT NULL,  -- false: the object has been withdrawn
    geom      geography(Point, 4326),
    symbol    text,
    speed     real,
    course    real,
    weather   jsonb,
    addressee text,              -- messages: a callsign, or a group like BLN1 / NWS-WARN
    text      text    NOT NULL,  -- comment, status or message text
    raw       text    NOT NULL
);
CREATE INDEX aprs_packet_time ON aprs_packet USING brin (time);
CREATE INDEX aprs_packet_name ON aprs_packet (name, time DESC);

-- What belongs on the map: where each station or object was last reported,
-- unless that last report withdrew it.
CREATE VIEW aprs_station AS
SELECT * FROM (
    SELECT DISTINCT ON (name) * FROM aprs_packet WHERE geom IS NOT NULL ORDER BY name, time DESC
) latest
WHERE alive;

-- Messages meant for everyone: bulletins, announcements, weather-service
-- alerts. Senders repeat them, so keep the latest of each.
CREATE VIEW aprs_bulletin AS
SELECT DISTINCT ON (callsign, addressee) time, callsign, addressee, text
FROM aprs_packet
WHERE kind = 'message' AND addressee ~ '^(BLN|NWS|SKY|CWA|ALL)'
ORDER BY callsign, addressee, time DESC;
