//! Plain-text and Markdown tables, multi-section documents, and CSV.

use serde::Serialize;

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

    pub fn is_empty(&self) -> bool {
        !self.rows.iter().any(|r| matches!(r, Row::Cells(_)))
    }

    /// Renders as plain text, or as a Markdown table.
    pub fn render_as(&self, markdown: bool) -> String {
        if markdown {
            self.render_markdown()
        } else {
            self.render()
        }
    }

    pub fn render_markdown(&self) -> String {
        let escape = |s: &str| s.replace('|', "\\|");
        let line = |cells: Vec<String>| format!("| {} |\n", cells.join(" | "));
        let mut out = line(self.columns.iter().map(|(h, _)| escape(h)).collect());
        out += &line(
            self.columns
                .iter()
                .map(|(_, align)| match align {
                    Align::Left => "---".to_string(),
                    Align::Right => "---:".to_string(),
                })
                .collect(),
        );
        for row in &self.rows {
            if let Row::Cells(cells) = row {
                out += &line(cells.iter().map(|c| escape(c)).collect());
            }
        }
        out
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

/// A report with headings, text and tables, rendered as plain text or Markdown.
#[derive(Default)]
pub struct Document {
    parts: Vec<Part>,
}

enum Part {
    Title(String),
    Heading(String),
    Text(String),
    Table(Table),
}

impl Document {
    pub fn title(&mut self, text: impl Into<String>) {
        self.parts.push(Part::Title(text.into()));
    }

    pub fn heading(&mut self, text: impl Into<String>) {
        self.parts.push(Part::Heading(text.into()));
    }

    pub fn text(&mut self, text: impl Into<String>) {
        self.parts.push(Part::Text(text.into()));
    }

    pub fn table(&mut self, table: Table) {
        self.parts.push(Part::Table(table));
    }

    pub fn render(&self, markdown: bool) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Title(t) if markdown => out += &format!("# {t}\n\n"),
                Part::Title(t) => out += &format!("{t}\n{}\n\n", "=".repeat(t.chars().count())),
                Part::Heading(h) if markdown => out += &format!("## {h}\n\n"),
                Part::Heading(h) => out += &format!("\n{h}\n{}\n", "-".repeat(h.chars().count())),
                Part::Text(t) if markdown => out += &format!("{t}\n\n"),
                Part::Text(t) => out += &format!("{t}\n"),
                Part::Table(t) => {
                    out += &t.render_as(markdown);
                    if markdown {
                        out.push('\n');
                    }
                }
            }
        }
        out
    }
}

/// Renders serializable rows as CSV: one column per field of the first row.
/// Nested values are written as JSON.
pub fn csv_from_rows<T: Serialize>(rows: &[T]) -> serde_json::Result<String> {
    let values: Vec<serde_json::Value> = rows
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()?;
    let Some(serde_json::Value::Object(first)) = values.first() else {
        return Ok(String::new());
    };
    let headers: Vec<String> = first.keys().cloned().collect();
    let mut out = csv_row(&headers);
    out.push('\n');
    for value in &values {
        let cells: Vec<String> = headers
            .iter()
            .map(|h| match value.get(h) {
                None | Some(serde_json::Value::Null) => String::new(),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
            })
            .collect();
        out += &csv_row(&cells);
        out.push('\n');
    }
    Ok(out)
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
