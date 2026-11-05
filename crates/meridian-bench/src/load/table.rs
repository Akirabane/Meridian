//! The figures of a run as rows: written as CSV for the repository, and as a Markdown table for
//! the report. Each row is a list of named columns; a table takes its header from the first row.
use std::io;
use std::path::Path;

/// One row: column names and their values, in order.
#[derive(Clone, Debug, Default)]
pub struct Row(pub Vec<(&'static str, String)>);

impl Row {
    pub fn new() -> Row {
        Row(Vec::new())
    }

    /// Adds a column.
    pub fn with(mut self, name: &'static str, value: impl ToString) -> Row {
        self.0.push((name, value.to_string()));
        self
    }

    /// Adds a column with a number of decimals.
    pub fn num(self, name: &'static str, value: f64, decimals: usize) -> Row {
        self.with(name, format!("{value:.decimals$}"))
    }
}

/// The rows of one scenario.
#[derive(Clone, Debug, Default)]
pub struct Table {
    pub rows: Vec<Row>,
}

impl Table {
    pub fn push(&mut self, row: Row) {
        self.rows.push(row);
    }

    /// The CSV text: a header, then one line a row. A value with a comma or a quote is quoted.
    pub fn csv(&self) -> String {
        let Some(first) = self.rows.first() else {
            return String::new();
        };
        let quote = |v: &str| {
            if v.contains([',', '"', '\n']) {
                format!("\"{}\"", v.replace('"', "\"\""))
            } else {
                v.to_owned()
            }
        };
        let mut out = first
            .0
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(",");
        out.push('\n');
        for row in &self.rows {
            out.push_str(
                &row.0
                    .iter()
                    .map(|(_, v)| quote(v))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push('\n');
        }
        out
    }

    /// A Markdown table with a column for each name of `columns` that the rows have.
    pub fn markdown(&self, columns: &[&str]) -> String {
        let mut out = format!("| {} |\n", columns.join(" | "));
        out.push_str(&format!("|{}\n", " --- |".repeat(columns.len())));
        for row in &self.rows {
            let cells: Vec<&str> = columns
                .iter()
                .map(|c| {
                    row.0
                        .iter()
                        .find(|(name, _)| name == c)
                        .map_or("", |(_, v)| v.as_str())
                })
                .collect();
            out.push_str(&format!("| {} |\n", cells.join(" | ")));
        }
        out
    }

    pub fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.csv())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_become_csv_and_markdown() {
        let mut table = Table::default();
        table.push(Row::new().with("players", 100).num("cpu", 12.345, 1));
        table.push(Row::new().with("players", 250).with("cpu", "a,b"));
        assert_eq!(table.csv(), "players,cpu\n100,12.3\n250,\"a,b\"\n");
        assert_eq!(
            table.markdown(&["players", "cpu", "absent"]),
            "| players | cpu | absent |\n| --- | --- | --- |\n| 100 | 12.3 |  |\n| 250 | a,b |  |\n"
        );
        assert_eq!(Table::default().csv(), "");
    }
}
