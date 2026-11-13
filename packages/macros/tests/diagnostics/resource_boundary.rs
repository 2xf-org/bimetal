//! A resource crosses as one without an attribute, and only a resource
//! crosses as `&mut T` or `Option<&T>`.

pub struct Document;

#[bimetal::export]
impl Document {
    #[must_use]
    pub fn fork(&self) -> Self {
        Self
    }
}

#[bimetal::export]
#[must_use]
pub fn pages(#[bimetal(resource)] _document: &Document) -> u32 {
    1
}

#[bimetal::export]
pub fn touch(count: &mut u32) {
    *count += 1;
}

fn main() {}
