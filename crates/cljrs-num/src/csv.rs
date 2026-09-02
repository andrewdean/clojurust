//! Bulk CSV emission: format whole columns in Rust and write the file in one
//! call, instead of assembling rows cell-by-cell in the interpreter.
//!
//! `(cljrs.num/write-csv! path header cols)` — `cols` is a vector of column
//! specs, each a vector tagged by its first element:
//!
//!   [:d double-array]   doubles, shortest round-trip repr; NaN → empty field
//!   [:l long-array]     longs
//!   [:date long-array]  epoch-day numbers rendered as YYYY-MM-DD
//!   [:ts days secs]     two long-arrays rendered as "YYYY-MM-DD HH:MM:SS"
//!   [:s vec]            per-row strings (nil → empty), CSV-escaped
//!   [:const s]          the same string for every row (nil → empty)
//!
//! All non-const columns must share one length; that length is the row
//! count. A fourth argument `true` appends to an existing file (the header
//! is only written when the file is created).

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

use cljrs_value::Value;

enum Column {
    /// (epoch-day, seconds-of-day) pairs rendered as "YYYY-MM-DD HH:MM:SS";
    /// seconds past 86400 carry into the next day.
    Timestamps(Vec<i64>, Vec<i64>),
    Doubles(Vec<f64>),
    Longs(Vec<i64>),
    Dates(Vec<i64>),
    Strings(Vec<Option<String>>),
    Const(String),
}

impl Column {
    fn len(&self) -> Option<usize> {
        match self {
            Column::Doubles(v) => Some(v.len()),
            Column::Longs(v) | Column::Dates(v) => Some(v.len()),
            Column::Timestamps(d, _) => Some(d.len()),
            Column::Strings(v) => Some(v.len()),
            Column::Const(_) => None,
        }
    }
}

fn escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Howard Hinnant's civil_from_days.
fn day_to_ymd(day: i64) -> (i64, i64, i64) {
    let z = day + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn day_str(day: i64) -> String {
    let (y, m, d) = day_to_ymd(day);
    format!("{y:04}-{m:02}-{d:02}")
}

fn ts_str(day: i64, secs: i64) -> String {
    let day = day + secs.div_euclid(86_400);
    let secs = secs.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02}:{:02}",
        day_str(day),
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn string_cell(v: &Value) -> Result<Option<String>, String> {
    match v {
        Value::Nil => Ok(None),
        Value::Str(s) => Ok(Some(s.get().clone())),
        Value::Bool(b) => Ok(Some(b.to_string())),
        Value::Long(n) => Ok(Some(n.to_string())),
        Value::Double(x) => Ok(Some(x.to_string())),
        other => Err(format!(
            "string column cells must be string/bool/number/nil, got {}",
            other.type_name()
        )),
    }
}

fn parse_column(spec: &Value) -> Result<Column, String> {
    let items: Vec<Value> = match spec {
        Value::Vector(v) => v.get().iter().cloned().collect(),
        other => {
            return Err(format!(
                "column spec must be a vector, got {}",
                other.type_name()
            ));
        }
    };
    let tag = match items.first() {
        Some(Value::Keyword(k)) => k.get().name.to_string(),
        _ => return Err("column spec must start with a keyword tag".to_string()),
    };
    let payload = items.get(1);
    match (tag.as_str(), payload) {
        ("d", Some(Value::DoubleArray(a))) => Ok(Column::Doubles(a.get().lock().unwrap().clone())),
        ("l", Some(Value::LongArray(a))) => Ok(Column::Longs(a.get().lock().unwrap().clone())),
        ("date", Some(Value::LongArray(a))) => Ok(Column::Dates(a.get().lock().unwrap().clone())),
        ("ts", Some(Value::LongArray(d))) => match items.get(2) {
            Some(Value::LongArray(sec)) => {
                let days = d.get().lock().unwrap().clone();
                let secs = sec.get().lock().unwrap().clone();
                if days.len() != secs.len() {
                    return Err(format!(
                        "[:ts] day/second arrays differ: {} vs {}",
                        days.len(),
                        secs.len()
                    ));
                }
                Ok(Column::Timestamps(days, secs))
            }
            _ => Err("[:ts] needs two long-arrays: epoch-days and seconds-of-day".to_string()),
        },
        ("s", Some(Value::Vector(v))) => {
            let mut out = Vec::with_capacity(v.get().count());
            for cell in v.get().iter() {
                out.push(string_cell(cell)?);
            }
            Ok(Column::Strings(out))
        }
        ("const", Some(Value::Nil)) | ("const", None) => Ok(Column::Const(String::new())),
        ("const", Some(Value::Str(s))) => Ok(Column::Const(s.get().clone())),
        ("const", Some(other)) => Ok(Column::Const(string_cell(other)?.unwrap_or_default())),
        (t, Some(p)) => Err(format!(
            "bad column spec [:{t} {}]: expected [:d double-array], [:l long-array], \
             [:date long-array], [:s vector], or [:const string]",
            p.type_name()
        )),
        (t, None) => Err(format!("column spec [:{t}] is missing its payload")),
    }
}

pub fn write_csv(
    path: &str,
    header: &[String],
    col_specs: &[Value],
    append: bool,
) -> Result<i64, String> {
    let cols: Vec<Column> = col_specs
        .iter()
        .map(parse_column)
        .collect::<Result<_, _>>()?;
    if cols.len() != header.len() {
        return Err(format!(
            "{} header names but {} columns",
            header.len(),
            cols.len()
        ));
    }

    let mut n_rows: Option<usize> = None;
    for (i, c) in cols.iter().enumerate() {
        if let Some(len) = c.len() {
            match n_rows {
                None => n_rows = Some(len),
                Some(n) if n != len => {
                    return Err(format!(
                        "column {} ({}) has {} rows, expected {}",
                        i, header[i], len, n
                    ));
                }
                _ => {}
            }
        }
    }
    let n_rows = n_rows.ok_or("at least one non-const column is required")?;

    // Pre-render every column to strings once, then join row-wise.
    let rendered: Vec<Vec<String>> = cols
        .iter()
        .map(|c| match c {
            Column::Doubles(v) => v
                .iter()
                .map(|x| {
                    if x.is_nan() {
                        String::new()
                    } else {
                        x.to_string()
                    }
                })
                .collect(),
            Column::Longs(v) => v.iter().map(|x| x.to_string()).collect(),
            Column::Dates(v) => v.iter().map(|&d| day_str(d)).collect(),
            Column::Timestamps(d, sec) => d
                .iter()
                .zip(sec.iter())
                .map(|(&day, &s)| ts_str(day, s))
                .collect(),
            Column::Strings(v) => v
                .iter()
                .map(|s| s.as_deref().map(escape).unwrap_or_default())
                .collect(),
            Column::Const(s) => vec![escape(s)],
        })
        .collect();

    let exists = Path::new(path).exists();
    let file = OpenOptions::new()
        .create(true)
        .append(append)
        .write(true)
        .truncate(!append)
        .open(path)
        .map_err(|e| format!("open {path}: {e}"))?;
    let mut out = std::io::BufWriter::with_capacity(1 << 20, file);

    if !(append && exists) {
        writeln!(
            out,
            "{}",
            header
                .iter()
                .map(|h| escape(h))
                .collect::<Vec<_>>()
                .join(",")
        )
        .map_err(|e| format!("write {path}: {e}"))?;
    }

    let mut line = String::with_capacity(128);
    for row in 0..n_rows {
        line.clear();
        for (i, col) in rendered.iter().enumerate() {
            if i > 0 {
                line.push(',');
            }
            let cell = if col.len() == 1 && cols[i].len().is_none() {
                &col[0]
            } else {
                &col[row]
            };
            let _ = write!(line, "{cell}");
        }
        writeln!(out, "{line}").map_err(|e| format!("write {path}: {e}"))?;
    }
    out.flush().map_err(|e| format!("flush {path}: {e}"))?;
    Ok(n_rows as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_formatting() {
        assert_eq!(day_str(18262), "2020-01-01");
        assert_eq!(day_str(0), "1970-01-01");
        assert_eq!(day_str(20088), "2024-12-31");
    }

    #[test]
    fn escaping() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("a,b"), "\"a,b\"");
        assert_eq!(escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
