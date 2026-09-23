//! Track import (GPX, GeoJSON) and export (GPX). GeoJSON export is done by
//! PostGIS directly.

use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub name: Option<String>,
    pub points: Vec<Point>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub lon: f64,
    pub lat: f64,
    pub ele: Option<f64>,
    pub time: Option<OffsetDateTime>,
}

/// Parse a GPX or GeoJSON document, whichever it turns out to be. Tracks with
/// fewer than two points are dropped.
pub fn parse(doc: &str) -> Result<Vec<Track>, String> {
    let doc = doc.strip_prefix('\u{feff}').unwrap_or(doc);
    let mut tracks = if doc.trim_start().starts_with('<') {
        parse_gpx(doc)?
    } else {
        let json = serde_json::from_str(doc).map_err(|e| e.to_string())?;
        let mut tracks = Vec::new();
        collect_geojson(&json, None, &mut tracks);
        tracks
    };
    let valid = |p: &Point| (-90.0..=90.0).contains(&p.lat) && (-180.0..=180.0).contains(&p.lon);
    tracks.retain(|t| t.points.len() > 1 && t.points.iter().all(valid));
    Ok(tracks)
}

/// Each `<trkseg>` and each `<rte>` becomes a track: a new segment means the
/// receiver lost its fix or was switched off, which is where we split too.
fn parse_gpx(doc: &str) -> Result<Vec<Track>, String> {
    let xml = roxmltree::Document::parse(doc).map_err(|e| e.to_string())?;
    let child_text = |node: roxmltree::Node, tag: &str| {
        let child = node.children().find(|c| c.has_tag_name(tag))?;
        child.text().map(str::trim).map(str::to_owned)
    };
    let tracks = xml.descendants().filter(|n| n.has_tag_name("trkseg") || n.has_tag_name("rte"));
    Ok(tracks
        .map(|seg| {
            let named = if seg.has_tag_name("rte") { Some(seg) } else { seg.parent() };
            let points = seg.children().filter(|n| n.has_tag_name("trkpt") || n.has_tag_name("rtept"));
            Track {
                name: named.and_then(|n| child_text(n, "name")),
                points: points
                    .filter_map(|pt| {
                        Some(Point {
                            lon: pt.attribute("lon")?.parse().ok()?,
                            lat: pt.attribute("lat")?.parse().ok()?,
                            ele: child_text(pt, "ele").and_then(|e| e.parse().ok()),
                            time: child_text(pt, "time").and_then(|t| OffsetDateTime::parse(&t, &Rfc3339).ok()),
                        })
                    })
                    .collect(),
            }
        })
        .collect())
}

/// Walks FeatureCollection / Feature / geometry, taking every line it finds.
fn collect_geojson(v: &Value, name: Option<&str>, out: &mut Vec<Track>) {
    let line = |coords: &Value| Track {
        name: name.map(str::to_owned),
        points: coords
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                Some(Point { lon: c.get(0)?.as_f64()?, lat: c.get(1)?.as_f64()?, ele: c.get(2).and_then(Value::as_f64), time: None })
            })
            .collect(),
    };
    let each = |key: &str| v[key].as_array().into_iter().flatten();
    match v["type"].as_str() {
        Some("FeatureCollection") => each("features").for_each(|f| collect_geojson(f, name, out)),
        Some("GeometryCollection") => each("geometries").for_each(|g| collect_geojson(g, name, out)),
        Some("Feature") => collect_geojson(&v["geometry"], v["properties"]["name"].as_str().or(name), out),
        Some("LineString") => out.push(line(&v["coordinates"])),
        Some("MultiLineString") => out.extend(each("coordinates").map(line)),
        _ => {}
    }
}

/// Write one track as GPX 1.1.
pub fn to_gpx(name: Option<&str>, points: &[Point]) -> String {
    let mut gpx = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <gpx version=\"1.1\" creator=\"carchomp\" xmlns=\"http://www.topografix.com/GPX/1/1\">\n<trk>\n",
    );
    if let Some(name) = name {
        // Control characters other than tab/newline, and U+FFFE/U+FFFF, are not XML 1.0 Chars.
        let name: String = name
            .chars()
            .filter(|&c| (c >= ' ' || matches!(c, '\t' | '\n' | '\r')) && !matches!(c, '\u{fffe}' | '\u{ffff}'))
            .collect();
        let escaped = name.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        gpx += &format!("<name>{escaped}</name>\n");
    }
    gpx += "<trkseg>\n";
    for p in points {
        gpx += &format!("<trkpt lat=\"{:.6}\" lon=\"{:.6}\">", p.lat, p.lon);
        if let Some(ele) = p.ele {
            gpx += &format!("<ele>{ele:.1}</ele>");
        }
        if let Some(time) = p.time.and_then(|t| t.format(&Rfc3339).ok()) {
            gpx += &format!("<time>{time}</time>");
        }
        gpx += "</trkpt>\n";
    }
    gpx + "</trkseg>\n</trk>\n</gpx>\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn gpx_round_trip() {
        let points = vec![
            Point { lon: -122.6587, lat: 45.5122, ele: Some(15.0), time: Some(datetime!(2026-09-17 20:00 UTC)) },
            Point { lon: -122.6500, lat: 45.5130, ele: None, time: None },
        ];
        let tracks = parse(&to_gpx(Some("Fish & <Chips>"), &points)).unwrap();
        assert_eq!(tracks, [Track { name: Some("Fish & <Chips>".into()), points }]);
    }

    #[test]
    fn gpx_with_bom_is_still_gpx() {
        let pt = r#"<trkpt lat="1" lon="2"/>"#;
        let doc = format!("\u{feff}<?xml version=\"1.0\"?><gpx><trk><trkseg>{pt}{pt}</trkseg></trk></gpx>");
        assert_eq!(parse(&doc).unwrap().len(), 1);
    }

    #[test]
    fn control_characters_in_name_are_dropped() {
        let points = vec![Point { lon: 1.0, lat: 2.0, ele: None, time: None }, Point { lon: 3.0, lat: 4.0, ele: None, time: None }];
        let tracks = parse(&to_gpx(Some("Trip\u{1}\u{ffff}\u{7f}"), &points)).unwrap();
        assert_eq!(tracks[0].name.as_deref(), Some("Trip\u{7f}"));
    }

    #[test]
    fn gpx_segments_become_tracks_and_stubs_are_dropped() {
        let pt = r#"<trkpt lat="1" lon="2"/>"#;
        let doc = format!("<gpx><trk><name>n</name><trkseg>{pt}{pt}</trkseg><trkseg>{pt}</trkseg><trkseg>{pt}{pt}{pt}</trkseg></trk></gpx>");
        let lens: Vec<_> = parse(&doc).unwrap().iter().map(|t| t.points.len()).collect();
        assert_eq!(lens, [2, 3]);
    }

    #[test]
    fn geojson_lines_at_any_depth() {
        let doc = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"name":"a"},"geometry":{"type":"LineString","coordinates":[[1,2,3],[4,5]]}},
            {"type":"Feature","properties":{},"geometry":{"type":"MultiLineString","coordinates":[[[1,2],[3,4]],[[5,6],[7,8]]]}},
            {"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;
        let tracks = parse(doc).unwrap();
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].name.as_deref(), Some("a"));
        assert_eq!(tracks[0].points[0].ele, Some(3.0));
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("<gpx><unclosed>").is_err());
        assert!(parse("not json").is_err());
        assert_eq!(parse(r#"{"type":"LineString","coordinates":[[0,95],[0,96]]}"#).unwrap(), []);
    }
}
