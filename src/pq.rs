//! Minimal Parquet writer for our two output tables (low-level API, no arrow).
//! Column kinds: Utf8 (optional or required), DATE (INT32 logical date), INT64.

use parquet::basic::Compression;
use parquet::data_type::ByteArray;
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;
use std::fs::File;
use std::sync::Arc;

pub enum Column {
    /// Required string column.
    Str(Vec<ByteArray>),
    /// Optional string column: `None` is null.
    OptStr(Vec<Option<ByteArray>>),
    /// DATE: days since 1970-01-01.
    Date(Vec<i32>),
    Int64(Vec<i64>),
}

impl Column {
    fn len(&self) -> usize {
        match self {
            Column::Str(v) => v.len(),
            Column::OptStr(v) => v.len(),
            Column::Date(v) => v.len(),
            Column::Int64(v) => v.len(),
        }
    }
}

const ROW_GROUP: usize = 1 << 20;

pub fn write_table(path: &str, schema: &str, cols: &[Column]) -> Result<(), String> {
    let e = |x: parquet::errors::ParquetError| x.to_string();
    let schema = Arc::new(parse_message_type(schema).map_err(e)?);
    let props = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::UNCOMPRESSED)
            .set_dictionary_enabled(false)
            .set_statistics_enabled(parquet::file::properties::EnabledStatistics::None)
            .build(),
    );
    let file = File::create(path).map_err(|x| x.to_string())?;
    let mut w = SerializedFileWriter::new(file, schema, props).map_err(e)?;
    let n = cols.first().map_or(0, |c| c.len());
    let mut start = 0;
    loop {
        // an empty table still gets one (empty) row group
        let end = (start + ROW_GROUP).min(n);
        let mut rg = w.next_row_group().map_err(e)?;
        for col in cols {
            let mut cw = rg.next_column().map_err(e)?.ok_or("schema/column count mismatch")?;
            {
                use parquet::column::writer::ColumnWriter as CW;
                match (cw.untyped(), col) {
                    (CW::ByteArrayColumnWriter(w), Column::Str(v)) => {
                        w.write_batch(&v[start..end], None, None).map_err(e)?;
                    }
                    (CW::ByteArrayColumnWriter(w), Column::OptStr(v)) => {
                        let s = &v[start..end];
                        let vals: Vec<ByteArray> = s.iter().flatten().cloned().collect();
                        let def: Vec<i16> = s.iter().map(|x| x.is_some() as i16).collect();
                        w.write_batch(&vals, Some(&def), None).map_err(e)?;
                    }
                    (CW::Int32ColumnWriter(w), Column::Date(v)) => {
                        w.write_batch(&v[start..end], None, None).map_err(e)?;
                    }
                    (CW::Int64ColumnWriter(w), Column::Int64(v)) => {
                        w.write_batch(&v[start..end], None, None).map_err(e)?;
                    }
                    _ => return Err("column type mismatch".into()),
                }
            }
            cw.close().map_err(e)?;
        }
        rg.close().map_err(e)?;
        start = end;
        if start >= n {
            break;
        }
    }
    w.close().map_err(e)?;
    Ok(())
}
