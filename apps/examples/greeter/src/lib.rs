//! A small Bimetal application: one Rust API that Python and TypeScript call.

pub mod stats;

use bimetal::Model;
use serde::{Deserialize, Serialize};

/// The most names one guestbook holds.
#[bimetal::export]
pub const GUESTBOOK_CAPACITY: u32 = 3;

/// How warmly to greet someone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// "Hi, Ada."
    Casual,
    /// "Good day, Ada."
    Formal,
}

/// A request to greet one person.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct GreetingRequest {
    /// The person to greet.
    #[bimetal(min_length = 1, max_length = 64)]
    pub name: String,
    /// How warmly to greet them.
    pub tone: Tone,
    /// End the greeting with an exclamation mark.
    #[serde(default)]
    pub excited: bool,
}

/// A greeting ready to show.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct Greeting {
    /// The full greeting text.
    pub message: String,
    /// The tone the greeting used.
    pub tone: Tone,
}

/// Failures that callers handle by their stable `code`.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum GreeterError {
    /// The name contained only whitespace.
    #[error("the name is blank")]
    BlankName,
    /// The guestbook already holds `GUESTBOOK_CAPACITY` names.
    #[error("the guestbook is full")]
    GuestbookFull,
}

/// Greet one person.
#[bimetal::export]
pub fn greet(request: GreetingRequest) -> Result<Greeting, GreeterError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(GreeterError::BlankName);
    }
    let salutation = match request.tone {
        Tone::Casual => "Hi",
        Tone::Formal => "Good day",
    };
    let ending = if request.excited { "!" } else { "." };
    Ok(Greeting {
        message: format!("{salutation}, {name}{ending}"),
        tone: request.tone,
    })
}

/// The names signed for one event, kept in Rust between host calls.
pub struct Guestbook {
    event: String,
    names: Vec<String>,
}

#[bimetal::export]
impl Guestbook {
    /// Open an empty guestbook.
    #[bimetal(constructor)]
    #[must_use]
    pub fn new(event: String) -> Self {
        Self {
            event,
            names: Vec::new(),
        }
    }

    /// Add one name.
    pub fn sign(&mut self, name: String) -> Result<(), GreeterError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(GreeterError::BlankName);
        }
        if self.names.len() >= GUESTBOOK_CAPACITY as usize {
            return Err(GreeterError::GuestbookFull);
        }
        self.names.push(name.to_owned());
        Ok(())
    }

    /// The event this guestbook belongs to.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }

    /// The names signed so far, in signing order.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }
}
