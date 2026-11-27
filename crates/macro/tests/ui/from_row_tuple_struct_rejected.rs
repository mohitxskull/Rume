use rume_macros::FromRow;

#[derive(FromRow)]
struct BadTuple(String, i32);

fn main() {}
