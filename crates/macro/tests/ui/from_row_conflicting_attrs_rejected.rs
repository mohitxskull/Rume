use rume_macros::FromRow;

#[derive(FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct BadAttrs {
    #[from_row(skip, rename = "x")]
    field: String,
}

fn main() {}
