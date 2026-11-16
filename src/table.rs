//! Plain-text tables and CSV rows.

#[derive(Clone, Copy, Debug)]
pub enum Align {
    Left,
    Right,
}

enum Row {
    Cells(Vec<String>),
    Rule,
}

/// A table rendered with space-padded columns.
pub struct Table {
    columns: Vec<(String, Align)>,
    rows: Vec<Row>,
}

const GAP: &str = "   ";

impl Table {
    pub fn new(columns: &[(&str, Align)]) -> Self {
        Self {
            columns: columns
                .iter()
                .map(|(name, align)| ((*name).to_string(), *align))
                .collect(),
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: Vec<String>) {
        debug_assert_eq!(cells.len(), self.columns.len());
        self.rows.push(Row::Cells(cells));
    }

    /// Adds a horizontal separator line.
    pub fn rule(&mut self) {
        self.rows.push(Row::Rule);
    }

    pub fn render(&self) -> String {
        let mut widths: Vec<usize> = self
            .columns
            .iter()
            .map(|(h, _)| h.chars().count())
            .collect();
        for row in &self.rows {
            if let Row::Cells(cells) = row {
                for (w, cell) in widths.iter_mut().zip(cells) {
                    *w = (*w).max(cell.chars().count());
                }
            }
        }
        let total = widths.iter().sum::<usize>() + GAP.len() * widths.len().saturating_sub(1);

        let mut out = String::new();
        let headers: Vec<String> = self.columns.iter().map(|(h, _)| h.clone()).collect();
        self.push_line(&mut out, &headers, &widths);
        for row in &self.rows {
            match row {
                Row::Cells(cells) => self.push_line(&mut out, cells, &widths),
                Row::Rule => {
                    out.push_str(&"-".repeat(total));
                    out.push('\n');
                }
            }
        }
        out
    }

    fn push_line(&self, out: &mut String, cells: &[String], widths: &[usize]) {
        let mut line = String::new();
        for (i, ((cell, width), (_, align))) in
            cells.iter().zip(widths).zip(&self.columns).enumerate()
        {
            if i > 0 {
                line.push_str(GAP);
            }
            let pad = width.saturating_sub(cell.chars().count());
            match align {
                Align::Left => {
                    line.push_str(cell);
                    line.push_str(&" ".repeat(pad));
                }
                Align::Right => {
                    line.push_str(&" ".repeat(pad));
                    line.push_str(cell);
                }
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
}

/// Formats one CSV row, quoting fields that need it.
pub fn csv_row<S: AsRef<str>>(fields: &[S]) -> String {
    fields
        .iter()
        .map(|f| {
            let f = f.as_ref();
            if f.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", f.replace('"', "\"\""))
            } else {
                f.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_aligned_columns() {
        let mut t = Table::new(&[("NAME", Align::Left), ("SIZE", Align::Right)]);
        t.row(vec!["alpha".into(), "1.0 KiB".into()]);
        t.row(vec!["b".into(), "12 B".into()]);
        t.rule();
        t.row(vec!["TOTAL".into(), "1.0 KiB".into()]);
        assert_eq!(
            t.render(),
            "NAME       SIZE\nalpha   1.0 KiB\nb          12 B\n---------------\nTOTAL   1.0 KiB\n"
        );
    }

    #[test]
    fn quotes_csv_fields() {
        assert_eq!(
            csv_row(&["a", "b,c", "say \"hi\""]),
            "a,\"b,c\",\"say \"\"hi\"\"\""
        );
    }
}
