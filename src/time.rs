use crate::Result;

const SQLITE_MEMORY: &str = ":memory:";

// Keep the fractional digits out of SQLite's calendar conversion to preserve nanoseconds.
fn split_timestamp(time: &str) -> Option<(&str, u64, i32)> {
    let stem = time.get(..19)?;
    for (i, c) in stem.bytes().enumerate() {
        let valid = match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            _ => c.is_ascii_digit(),
        };
        if !valid {
            return None;
        }
    }
    let suffix = &time[19..];
    let zone_at = suffix.find(['Z', '+', '-'])?;
    let (fraction, zone) = suffix.split_at(zone_at);
    let nanos = if fraction.is_empty() {
        0
    } else {
        let digits = fraction.strip_prefix('.')?;
        if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u64>().ok()? * 10u64.pow((9 - digits.len()) as u32)
    };
    let offset = match zone.as_bytes() {
        [b'Z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2]
            if [h1, h2, m1, m2].into_iter().all(u8::is_ascii_digit) =>
        {
            let hours = i32::from((h1 - b'0') * 10 + h2 - b'0');
            let minutes = i32::from((m1 - b'0') * 10 + m2 - b'0');
            if hours > 23 || minutes > 59 {
                return None;
            }
            (hours * 3600 + minutes * 60) * if *sign == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    Some((stem, nanos, offset))
}

pub fn timestamps(times: &[&str]) -> Result<Vec<(u64, u64)>> {
    use std::fmt::Write as _;

    let mut sql = String::from("WITH dates(value,offset) AS (VALUES ");
    let mut fractions = Vec::with_capacity(times.len());
    for (i, time) in times.iter().enumerate() {
        let (stem, nanos, offset) = split_timestamp(time)
            .ok_or_else(|| format!("invalid RFC 3339 timestamp at record {i}"))?;
        fractions.push(nanos);
        if i != 0 {
            sql.push(',');
        }
        write!(sql, "('{stem}',{offset})")?;
    }
    sql.push_str(
        "), normalized AS (
        SELECT value, unixepoch(value) AS seconds, offset FROM dates
    ) SELECT CASE
        WHEN strftime('%Y-%m-%dT%H:%M:%S',seconds,'unixepoch')=value
          AND seconds-offset BETWEEN 0 AND 253402300799 THEN seconds-offset
        ELSE -1 END FROM normalized;\n",
    );
    let output = crate::sqlite(std::path::Path::new(SQLITE_MEMORY), &sql, false)?;
    if output.lines().count() != fractions.len() {
        return Err("incomplete timestamp conversion".into());
    }
    output
        .lines()
        .zip(fractions)
        .map(|(seconds, nanos)| {
            Ok((
                seconds
                    .parse()
                    .map_err(|_| "invalid or out-of-range RFC 3339 date")?,
                nanos,
            ))
        })
        .collect()
}

pub fn iso(values: &[(u64, u64)]) -> Result<Vec<String>> {
    let mut sql = String::from("WITH dates(seconds,nanos) AS (VALUES ");
    for (index, (secs, nanos)) in values.iter().enumerate() {
        if index != 0 {
            sql.push(',');
        }
        sql.push_str(&format!("({secs},{nanos})"));
    }
    sql.push_str(") SELECT strftime('%Y-%m-%dT%H:%M:%S',seconds,'unixepoch') || printf('.%09dZ',nanos) FROM dates;\n");
    let output = crate::sqlite(std::path::Path::new(SQLITE_MEMORY), &sql, false)?;
    if output.lines().count() != values.len() || output.lines().any(str::is_empty) {
        return Err("invalid protobuf timestamp".into());
    }
    Ok(output.lines().map(str::to_owned).collect())
}
