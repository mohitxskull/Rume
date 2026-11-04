//! Table rendering helper using comfy-table.

use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};

/// Build and render a table.
pub struct TableBuilder {
    table: Table,
}

impl TableBuilder {
    pub fn new() -> Self {
        let mut table = Table::new();
        table.set_content_arrangement(ContentArrangement::Dynamic);
        Self { table }
    }

    pub fn head(&mut self, headers: &[&str]) -> &mut Self {
        let cells: Vec<Cell> = headers
            .iter()
            .map(|h| Cell::new(*h).add_attribute(Attribute::Bold))
            .collect();
        self.table.set_header(cells);
        self
    }

    pub fn row(&mut self, cells: &[&str]) -> &mut Self {
        self.table
            .add_row(cells.iter().map(|c| Cell::new(*c)).collect::<Vec<_>>());
        self
    }

    pub fn row_colored(&mut self, cells: &[(&str, Option<Color>)]) -> &mut Self {
        let row: Vec<Cell> = cells
            .iter()
            .map(|(text, color)| {
                let mut cell = Cell::new(*text);
                if let Some(c) = color {
                    cell = cell.fg(*c);
                }
                cell
            })
            .collect();
        self.table.add_row(row);
        self
    }

    pub fn render(&self) {
        println!("{}", self.table);
    }
}

impl Default for TableBuilder {
    fn default() -> Self {
        Self::new()
    }
}
