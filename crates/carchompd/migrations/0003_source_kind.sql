-- The kinds of source there are. 'import' is a file uploaded through the API.
-- A new kind of source adds itself here.
ALTER TABLE source ADD CONSTRAINT source_kind CHECK (kind IN ('gps', 'aprs', 'rf', 'camera', 'import'));
