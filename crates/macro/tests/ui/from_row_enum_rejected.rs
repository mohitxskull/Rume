use rume_macros::FromRow;

#[derive(FromRow)]
enum BadEnum {
    A,
    B,
}

fn main() {}
