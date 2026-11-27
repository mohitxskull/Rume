use rume_macros::FromRow;

#[derive(FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct Inner {
    id: i64,
}

#[derive(FromRow)]
#[from_row(crate = "rume_data::persistence")]
struct BadFlattenDefault {
    #[from_row(flatten, default)]
    inner: Inner,
}

fn main() {}
