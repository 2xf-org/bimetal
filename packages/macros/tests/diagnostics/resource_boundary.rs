//! The shapes that a resource can cross in, and what happens without the
//! attribute that says it is one.

pub struct Document;

#[bimetal::export]
impl Document {
    #[bimetal(returns(resource))]
    #[must_use]
    pub fn fork(&self) -> Self {
        Self
    }
}

pub struct Compiler;

#[bimetal::export]
impl Compiler {
    #[bimetal(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn compile(&self) -> Document {
        Document
    }
}

#[bimetal::export]
#[must_use]
pub fn pages(#[bimetal(resource)] document: Option<Document>) -> u32 {
    u32::from(document.is_some())
}

fn main() {}
