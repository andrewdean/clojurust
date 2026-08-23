//! `cljrsh.csv` — CSV read/write over the csv crate.

use cljrs_gc::GcPtr;
use cljrs_interop::{Registry, wrap_fn_variadic};
use cljrs_value::{PersistentVector, Value};

fn separator(args: &[Value]) -> Result<u8, String> {
    // Optional trailing `:separator \;` style option pairs.
    let mut sep = b',';
    let mut i = 1;
    while i + 1 < args.len() + 1 && i < args.len() {
        let Value::Keyword(k) = &args[i] else {
            return Err("options must be :keyword value pairs".to_string());
        };
        let val = args
            .get(i + 1)
            .ok_or_else(|| format!("option :{} is missing a value", k.get().name))?;
        match k.get().name.as_ref() {
            "separator" => {
                sep = match val {
                    Value::Char(c) if c.is_ascii() => *c as u8,
                    Value::Str(s) if s.get().len() == 1 => s.get().as_bytes()[0],
                    _ => return Err(":separator must be a single ASCII character".to_string()),
                };
            }
            other => return Err(format!("unknown option :{other}")),
        }
        i += 2;
    }
    Ok(sep)
}

pub fn register(registry: &mut Registry) {
    registry.define(
        "cljrsh.csv/read-csv",
        wrap_fn_variadic(
            "cljrsh.csv/read-csv",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let Value::Str(s) = &args[0] else {
                    return Err(format!(
                        "read-csv expects a string, got {}",
                        args[0].type_name()
                    ));
                };
                let sep = separator(args)?;
                let mut reader = csv::ReaderBuilder::new()
                    .has_headers(false)
                    .delimiter(sep)
                    .flexible(true)
                    .from_reader(s.get().as_bytes());
                let mut rows = Vec::new();
                for record in reader.records() {
                    let record = record.map_err(|e| format!("CSV parse error: {e}"))?;
                    rows.push(Value::Vector(GcPtr::new(PersistentVector::from_iter(
                        record.iter().map(|f| Value::string(f.to_string())),
                    ))));
                }
                Ok(Value::Vector(GcPtr::new(PersistentVector::from_iter(rows))))
            },
        ),
    );

    registry.define(
        "cljrsh.csv/write-csv-string",
        wrap_fn_variadic(
            "cljrsh.csv/write-csv-string",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let sep = separator(args)?;
                let mut writer = csv::WriterBuilder::new()
                    .delimiter(sep)
                    .from_writer(Vec::new());
                let rows: Vec<Value> = match &args[0] {
                    Value::Vector(v) => v.get().iter().cloned().collect(),
                    Value::List(l) => l.get().iter().cloned().collect(),
                    other => {
                        return Err(format!(
                            "write-csv-string expects a collection of rows, got {}",
                            other.type_name()
                        ));
                    }
                };
                for row in rows {
                    let fields: Vec<String> = match &row {
                        Value::Vector(v) => v
                            .get()
                            .iter()
                            .map(|f| match f {
                                Value::Str(s) => s.get().to_string(),
                                other => other.to_string(),
                            })
                            .collect(),
                        other => {
                            return Err(format!(
                                "each row must be a vector, got {}",
                                other.type_name()
                            ));
                        }
                    };
                    writer
                        .write_record(&fields)
                        .map_err(|e| format!("CSV write error: {e}"))?;
                }
                let bytes = writer
                    .into_inner()
                    .map_err(|e| format!("CSV write error: {e}"))?;
                Ok(Value::string(
                    String::from_utf8_lossy(&bytes).into_owned(),
                ))
            },
        ),
    );
}
