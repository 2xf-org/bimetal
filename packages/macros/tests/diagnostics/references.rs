//! A wrapper owns every argument, so it can lend only the outermost value,
//! and an async export cannot return a borrow.

#[bimetal::export]
#[must_use]
pub fn first(names: Option<&str>) -> String {
    names.unwrap_or_default().to_owned()
}

#[bimetal::export]
pub async fn echo(name: &str) -> &str {
    name
}

fn main() {}
