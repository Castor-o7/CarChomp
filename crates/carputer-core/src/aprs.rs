//! AX.25 UI frame decoding and APRS parsing (receive only).
//!
//! Understands what a car wants to know about: where stations are
//! (uncompressed, compressed and Mic-E positions), weather reports, objects and
//! items (how road closures, fires and other incidents are put on the map), and
//! messages (how bulletins and weather-service alerts are sent). Anything else
//! still yields a `Packet` of kind `Other`, and every packet keeps its full
//! TNC2 text so it can be re-parsed as coverage grows.

use crate::KNOT;
use serde::{Deserialize, Serialize};

const MPH: f64 = 0.447_04;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A station reporting where it is.
    Position,
    /// A station describing something else: `name` is that thing, and it is
    /// `alive` until the station says otherwise.
    Object,
    Weather,
    /// Text for `addressee`: a callsign, or a group such as `BLN1` or `NWS-WARN`.
    Message,
    Status,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Position => "position",
            Kind::Object => "object",
            Kind::Weather => "weather",
            Kind::Message => "message",
            Kind::Status => "status",
            Kind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Packet {
    /// Sending station with SSID, e.g. `N0CALL-9`.
    pub from: String,
    /// True if no digipeater relayed this: we heard the sender itself, so it
    /// is within radio range of us.
    pub direct: bool,
    /// The whole packet in TNC2 form, `FROM>TO,PATH:info`.
    pub raw: String,
    pub kind: Kind,
    /// What the packet is about: the object's name, otherwise the sender.
    pub name: String,
    /// False when an object has been withdrawn (closure lifted, fire out).
    pub alive: bool,
    pub position: Option<Position>,
    pub weather: Option<Weather>,
    pub addressee: Option<String>,
    /// Comment, status or message text.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub lat: f64,
    pub lon: f64,
    /// Symbol table and code, e.g. `/>` for a car.
    pub symbol: String,
    /// Metres per second.
    pub speed: Option<f64>,
    /// Degrees true.
    pub course: Option<f64>,
}

/// SI units throughout. Fields a station did not report are absent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Weather {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_dir: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gust: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rain_1h_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rain_24h_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humidity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pressure_hpa: Option<f64>,
}

/// Decode an AX.25 UI frame (as delivered by KISS) into an APRS packet.
pub fn parse_frame(frame: &[u8]) -> Option<Packet> {
    let mut addrs = Vec::new();
    let mut direct = true;
    let mut rest = frame;
    loop {
        let (addr, tail) = rest.split_at_checked(7)?;
        let mut call = callsign(addr)?;
        // The "has been repeated" bit, only meaningful on digipeater addresses.
        if addrs.len() >= 2 && addr[6] & 0x80 != 0 {
            call.push('*');
            direct = false;
        }
        addrs.push(call);
        rest = tail;
        if addr[6] & 1 == 1 {
            break;
        }
        if addrs.len() > 10 {
            return None;
        }
    }
    // Control 0x03 (UI), PID 0xF0 (no layer 3).
    let ([to, from, path @ ..], [0x03, 0xF0, info @ ..]) = (&addrs[..], rest) else {
        return None;
    };
    let header = [&[format!("{from}>{to}")], path].concat().join(",");
    let mut packet = parse_info(from, to, info);
    packet.direct = direct;
    packet.raw = format!("{header}:{}", text(info));
    Some(packet)
}

fn callsign(addr: &[u8]) -> Option<String> {
    let mut call: String = addr[..6].iter().map(|b| (b >> 1) as char).collect();
    call.truncate(call.trim_end().len());
    if call.is_empty() || !call.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    match (addr[6] >> 1) & 0x0F {
        0 => Some(call),
        ssid => Some(format!("{call}-{ssid}")),
    }
}

/// Parse an information field. `to` is needed because Mic-E hides the
/// latitude in the destination address.
pub fn parse_info(from: &str, to: &str, info: &[u8]) -> Packet {
    let blank = Packet {
        from: from.to_owned(),
        direct: true,
        raw: String::new(),
        kind: Kind::Other,
        name: from.to_owned(),
        alive: true,
        position: None,
        weather: None,
        addressee: None,
        text: String::new(),
    };
    // Filled in on a copy, so a packet that turns out to be malformed halfway
    // through is reported as `Other` with nothing half-set.
    let mut packet = blank.clone();
    match fill(&mut packet, to, info) {
        Some(()) => packet,
        None => Packet { text: text(info), ..blank },
    }
}

fn fill(p: &mut Packet, to: &str, info: &[u8]) -> Option<()> {
    let (&data_type, body) = info.split_first()?;
    match data_type {
        b'!' | b'=' => located(p, plain_or_compressed(body)?),
        // These carry a 7-byte timestamp first.
        b'/' | b'@' => located(p, plain_or_compressed(body.get(7..)?)?),
        b'`' | b'\'' => located(p, mic_e(to.as_bytes(), body)?),
        // ;NAME_____*DDHHMMz<position>   (`_` instead of `*`: withdrawn)
        b';' => {
            let (name, rest) = body.split_at_checked(9)?;
            let (&state, rest) = rest.split_first()?;
            object(p, name, state, rest.get(7..)?)
        }
        // )NAME!<position>   name is 3 to 9 characters
        b')' => {
            let end = body.iter().take(10).position(|&b| b == b'!' || b == b'_').filter(|&n| n >= 3)?;
            object(p, &body[..end], if body[end] == b'!' { b'*' } else { b'_' }, &body[end + 1..])
        }
        // :ADDRESSEE:text{id
        b':' => {
            let (addressee, rest) = body.split_at_checked(9)?;
            let message = text(rest.strip_prefix(b":")?);
            let message = message.rsplit_once('{').map_or(message.as_str(), |(m, _id)| m);
            if message.starts_with("ack") || message.starts_with("rej") {
                return None; // delivery receipts are noise to a listener
            }
            p.kind = Kind::Message;
            p.addressee = Some(text(addressee));
            p.text = message.trim().to_owned();
            Some(())
        }
        // _MMDDHHMMc...s...g...t...   weather without a position
        b'_' => {
            let (weather, comment) = weather(body.get(8..)?, None);
            p.kind = Kind::Weather;
            p.weather = Some(weather);
            p.text = text(comment);
            Some(())
        }
        b'>' => {
            p.kind = Kind::Status;
            p.text = text(body);
            Some(())
        }
        _ => None,
    }
}

/// A decoded position plus whatever followed it in the packet.
struct Located<'a> {
    position: Position,
    rest: &'a [u8],
    /// A `CSE/SPD` extension gives speed as a bare number whose unit depends
    /// on what the station is: knots for a vehicle, mph for a weather station.
    number: Option<f64>,
}

fn located(p: &mut Packet, mut at: Located) -> Option<()> {
    let Position { lat, lon, .. } = at.position;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    p.kind = Kind::Position;
    if at.position.symbol.ends_with('_') {
        // A weather station: its "course and speed" are the wind's.
        let speed = at.number.map(|n| n * MPH).or(at.position.speed);
        let wind = at.position.course.zip(speed);
        (at.position.course, at.position.speed) = (None, None);
        let (weather, comment) = weather(at.rest, wind);
        p.kind = Kind::Weather;
        p.weather = Some(weather);
        at.rest = comment;
    }
    p.text = text(at.rest);
    p.position = Some(at.position);
    Some(())
}

fn object(p: &mut Packet, name: &[u8], state: u8, position: &[u8]) -> Option<()> {
    located(p, plain_or_compressed(position)?)?;
    p.kind = Kind::Object;
    p.name = text(name);
    p.alive = state != b'_';
    (!p.name.is_empty()).then_some(())
}

fn plain_or_compressed(body: &[u8]) -> Option<Located<'_>> {
    if body.first()?.is_ascii_digit() {
        uncompressed(body)
    } else {
        compressed(body)
    }
}

/// `DDMM.hhN/DDDMM.hhW$` followed by an optional `CSE/SPD` and a comment.
fn uncompressed(b: &[u8]) -> Option<Located<'_>> {
    if b.len() < 19 {
        return None;
    }
    let lat = dm(&b[0..2], &b[2..7])? * hemisphere(b[7], b'N', b'S')?;
    let lon = dm(&b[9..12], &b[12..17])? * hemisphere(b[17], b'E', b'W')?;
    let mut rest = &b[19..];
    let (mut course, mut number) = (None, None);
    if let [c @ .., b'/', s1, s2, s3] = rest.get(..7).unwrap_or(&[]) {
        if let (Some(c), Some(s)) = (number_in(c), number_in(&[*s1, *s2, *s3])) {
            (course, number) = (Some(c), Some(s));
            rest = &rest[7..];
        }
    }
    let position = Position { lat, lon, symbol: symbol(b[8], b[18]), speed: number.map(|n| n * KNOT), course };
    Some(Located { position, rest, number })
}

/// `/YYYYXXXX$csT`: base-91 latitude and longitude.
fn compressed(b: &[u8]) -> Option<Located<'_>> {
    if b.len() < 13 {
        return None;
    }
    let lat = 90.0 - base91(&b[1..5])? / 380_926.0;
    let lon = -180.0 + base91(&b[5..9])? / 190_463.0;
    let (c, s) = (b[10], b[11]);
    let (course, speed) = if (b'!'..=b'z').contains(&c) && s >= b'!' {
        let knots = 1.08f64.powi((s - 33) as i32) - 1.0;
        (Some((c - 33) as f64 * 4.0), Some(knots * KNOT))
    } else {
        (None, None)
    };
    let position = Position { lat, lon, symbol: symbol(b[0], b[9]), speed, course };
    Some(Located { position, rest: &b[13..], number: None })
}

/// Mic-E: latitude lives in the 6-character destination callsign, the rest in
/// the info field. `body` is the info field after the data type byte.
fn mic_e<'a>(to: &[u8], b: &'a [u8]) -> Option<Located<'a>> {
    let to = to.get(..6)?;
    if b.len() < 8 {
        return None;
    }
    let mut digits = [0u8; 6];
    for (d, &c) in digits.iter_mut().zip(to) {
        *d = match c {
            b'0'..=b'9' => c - b'0',
            b'A'..=b'J' => c - b'A',
            b'P'..=b'Y' => c - b'P',
            b'K' | b'L' | b'Z' => 0, // position ambiguity
            _ => return None,
        };
    }
    // Characters 4-6 also carry N/S, the +100 longitude offset, and E/W.
    let flag = |c: u8| c >= b'P';
    let [d1, d2, m1, m2, h1, h2] = digits.map(f64::from);
    let lat = (d1 * 10.0 + d2) + (m1 * 10.0 + m2 + h1 / 10.0 + h2 / 100.0) / 60.0;
    let lat = if flag(to[3]) { lat } else { -lat };

    let v: Vec<i32> = b[..6].iter().map(|&c| c as i32 - 28).collect();
    if v.iter().any(|&x| x < 0) {
        return None;
    }
    let mut deg = v[0] + if flag(to[4]) { 100 } else { 0 };
    match deg {
        180..=189 => deg -= 80,
        190..=199 => deg -= 190,
        _ => {}
    }
    let min = if v[1] >= 60 { v[1] - 60 } else { v[1] };
    let lon = deg as f64 + (min as f64 + v[2] as f64 / 100.0) / 60.0;
    let lon = if flag(to[5]) { -lon } else { lon };

    let mut knots = v[3] * 10 + v[4] / 10;
    let mut course = v[4] % 10 * 100 + v[5];
    if knots >= 800 {
        knots -= 800;
    }
    if course >= 400 {
        course -= 400;
    }
    let position = Position { lat, lon, symbol: symbol(b[7], b[6]), speed: Some(knots as f64 * KNOT), course: Some(course as f64) };
    Some(Located { position, rest: &b[8..], number: None })
}

/// Weather fields are a run of `<letter><digits>` with fixed widths, e.g.
/// `g005t077r000h50b09900`. Returns the report and the unparsed remainder
/// (usually the station's software type). `wind` is direction and speed when
/// the position already supplied them.
fn weather(mut b: &[u8], wind: Option<(f64, f64)>) -> (Weather, &[u8]) {
    let mut w = Weather { wind_dir: wind.map(|w| w.0), wind_speed: wind.map(|w| w.1), ..Weather::default() };
    while let Some((&key, rest)) = b.split_first() {
        let width = match key {
            b'c' | b's' | b'g' | b't' | b'r' | b'p' | b'P' | b'L' | b'l' | b'#' => 3,
            b'h' => 2,
            b'b' => 5,
            _ => break,
        };
        let Some((digits, rest)) = rest.split_at_checked(width) else { break };
        // Unknown values are sent as dots or spaces; temperature can be negative.
        let value = std::str::from_utf8(digits).ok().and_then(|s| s.parse::<f64>().ok());
        if value.is_none() && !digits.iter().all(|&d| d == b'.' || d == b' ') {
            break;
        }
        match key {
            b'c' => w.wind_dir = value,
            b's' => w.wind_speed = value.map(|v| v * MPH),
            b'g' => w.gust = value.map(|v| v * MPH),
            b't' => w.temp_c = value.map(|f| (f - 32.0) / 1.8),
            b'r' => w.rain_1h_mm = value.map(|v| v * 0.254),
            b'p' => w.rain_24h_mm = value.map(|v| v * 0.254),
            b'h' => w.humidity = value.map(|v| if v == 0.0 { 100.0 } else { v }),
            b'b' => w.pressure_hpa = value.map(|v| v / 10.0),
            _ => {}
        }
        b = rest;
    }
    (w, b)
}

/// Degrees + `MM.hh` minutes. Spaces (position ambiguity) read as zero.
fn dm(deg: &[u8], min: &[u8]) -> Option<f64> {
    if min[2] != b'.' {
        return None;
    }
    let m = number_in(&min[..2])? + number_in(&min[3..])? / 100.0;
    Some(number_in(deg)? + m / 60.0)
}

fn number_in(digits: &[u8]) -> Option<f64> {
    digits.iter().try_fold(0.0, |acc, &d| match d {
        b'0'..=b'9' => Some(acc * 10.0 + (d - b'0') as f64),
        b' ' => Some(acc * 10.0),
        _ => None,
    })
}

fn hemisphere(c: u8, pos: u8, neg: u8) -> Option<f64> {
    if c == pos {
        Some(1.0)
    } else if c == neg {
        Some(-1.0)
    } else {
        None
    }
}

fn base91(b: &[u8]) -> Option<f64> {
    b.iter().try_fold(0.0, |acc, &c| (b'!'..=b'{').contains(&c).then(|| acc * 91.0 + (c - 33) as f64))
}

fn symbol(table: u8, code: u8) -> String {
    text(&[table, code])
}

/// NUL is legal on air but not in a PostgreSQL text column.
fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace('\0', "").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    fn parse(to: &str, info: &[u8]) -> Packet {
        parse_info("N0CALL", to, info)
    }

    #[test]
    fn uncompressed_with_course_and_speed() {
        let p = parse("APRS", b"!4903.50N/07201.75W>088/036 hello");
        let pos = p.position.unwrap();
        assert!(close(pos.lat, 49.058_333) && close(pos.lon, -72.029_167));
        assert_eq!((p.kind, pos.symbol.as_str(), pos.course), (Kind::Position, "/>", Some(88.0)));
        assert!(close(pos.speed.unwrap(), 36.0 * KNOT));
        assert_eq!((p.name.as_str(), p.text.as_str()), ("N0CALL", "hello"));
    }

    #[test]
    fn uncompressed_with_timestamp_and_no_extension() {
        let p = parse("APRS", b"@092345z4903.50N/07201.75W-Test");
        assert_eq!((p.position.unwrap().speed, p.text.as_str()), (None, "Test"));
    }

    #[test]
    fn compressed_spec_example() {
        // APRS101 ch. 9: 49 30' N, 72 45' W, course 88, 36.2 knots.
        let pos = parse("APRS", b"=/5L!!<*e7>7P[").position.unwrap();
        assert!(close(pos.lat, 49.5) && close(pos.lon, -72.75));
        assert_eq!((pos.symbol.as_str(), pos.course), ("/>", Some(88.0)));
        assert!((pos.speed.unwrap() / KNOT - 36.2).abs() < 0.1);
    }

    #[test]
    fn mic_e() {
        // Destination S32UVT: 33 25.64' N, +100 longitude offset, west.
        // Info (_f: 112 07.74' W. "n"O: 20 knots, course 251.
        let p = parse("S32UVT", b"`(_fn\"Oj/]hi");
        let pos = p.position.unwrap();
        assert!(close(pos.lat, 33.0 + 25.64 / 60.0) && close(pos.lon, -(112.0 + 7.74 / 60.0)));
        assert_eq!((pos.symbol.as_str(), pos.course, p.text.as_str()), ("/j", Some(251.0), "]hi"));
        assert!(close(pos.speed.unwrap(), 20.0 * KNOT));
    }

    #[test]
    fn weather_station() {
        // APRS101 ch. 12: wind 220 degrees at 4 mph gusting 5, 77 F, 50 %, 990.0 hPa.
        let p = parse("APRS", b"@092345z4903.50N/07201.75W_220/004g005t077r000p000P000h50b09900wRSW");
        let w = p.weather.unwrap();
        assert_eq!((p.kind, p.text.as_str()), (Kind::Weather, "wRSW"));
        assert_eq!((w.wind_dir, w.humidity, w.pressure_hpa, w.rain_1h_mm), (Some(220.0), Some(50.0), Some(990.0), Some(0.0)));
        assert!(close(w.wind_speed.unwrap(), 4.0 * MPH) && close(w.gust.unwrap(), 5.0 * MPH) && close(w.temp_c.unwrap(), 25.0));
        let pos = p.position.unwrap();
        assert_eq!((pos.speed, pos.course), (None, None));
    }

    #[test]
    fn weather_without_position_and_with_gaps() {
        let w = parse("APRS", b"_10090556c220s004g...t-07h00").weather.unwrap();
        assert_eq!((w.wind_dir, w.gust, w.humidity), (Some(220.0), None, Some(100.0)));
        assert!(close(w.temp_c.unwrap(), -21.666_7));
    }

    #[test]
    fn objects_and_items_are_named_things_that_can_be_withdrawn() {
        let p = parse("APRS", b";HWY26 CLS*092345z4530.00N/12130.00W'Closed at MP 42: crash");
        assert_eq!((p.kind, p.name.as_str(), p.alive), (Kind::Object, "HWY26 CLS", true));
        assert_eq!(p.text, "Closed at MP 42: crash");
        assert!(close(p.position.unwrap().lat, 45.5));

        let gone = parse("APRS", b";HWY26 CLS_092345z4530.00N/12130.00W'");
        assert_eq!((gone.name.as_str(), gone.alive), ("HWY26 CLS", false));

        let item = parse("APRS", b")FIRE!4530.00N/12130.00W: Brush fire");
        assert_eq!((item.kind, item.name.as_str(), item.alive, item.text.as_str()), (Kind::Object, "FIRE", true, "Brush fire"));
        assert!(!parse("APRS", b")FIRE_4530.00N/12130.00W:").alive);
    }

    #[test]
    fn messages_and_bulletins() {
        let p = parse("APRS", b":NWS-WARN :Red flag warning until 8PM PDT{ab12");
        assert_eq!((p.kind, p.addressee.as_deref(), p.text.as_str()), (Kind::Message, Some("NWS-WARN"), "Red flag warning until 8PM PDT"));
        assert_eq!(parse("APRS", b":BLN1     :Net tonight 7pm").addressee.as_deref(), Some("BLN1"));
        assert_eq!(parse("APRS", b":N0CALL-9 :ack12").kind, Kind::Other);
    }

    #[test]
    fn status_and_garbage() {
        let status = parse("APRS", b">On the road");
        assert_eq!((status.kind, status.text.as_str()), (Kind::Status, "On the road"));
        for junk in [&b""[..], b"!not a position at all", b"!9903.50N/07201.75W>", b";SHORT", b"?APRS?"] {
            let p = parse("APRS", junk);
            assert_eq!((p.kind, p.position, p.name.as_str()), (Kind::Other, None, "N0CALL"), "{junk:?}");
        }
    }

    #[test]
    fn ax25_frame() {
        fn addr(call: &str, ssid: u8, repeated: bool, last: bool) -> Vec<u8> {
            let mut a: Vec<u8> = format!("{call:<6}").bytes().map(|b| b << 1).collect();
            a.push(0x60 | (ssid << 1) | last as u8 | if repeated { 0x80 } else { 0 });
            a
        }
        let frame = |repeated| {
            let mut f = [addr("APRS", 0, false, false), addr("N0CALL", 9, false, false), addr("WIDE1", 1, repeated, true)].concat();
            f.extend_from_slice(b"\x03\xF0!4903.50N/07201.75W>");
            f
        };
        let p = parse_frame(&frame(false)).unwrap();
        assert_eq!((p.from.as_str(), p.direct, p.raw.as_str()), ("N0CALL-9", true, "N0CALL-9>APRS,WIDE1-1:!4903.50N/07201.75W>"));
        assert!(p.position.is_some());

        let relayed = parse_frame(&frame(true)).unwrap();
        assert_eq!((relayed.direct, relayed.raw.split(':').next()), (false, Some("N0CALL-9>APRS,WIDE1-1*")));
        assert!(parse_frame(&frame(false)[..10]).is_none());
    }
}
