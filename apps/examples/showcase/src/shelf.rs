//! Resources that make and take other resources, and the features of the host
//! languages that their methods implement.

use std::sync::{Arc, Mutex};
use std::vec;

use bimetal::{Callback, CallbackError};

use crate::pause;

/// The books of a shelf, which a session changes when it commits.
type Books = Arc<Mutex<Vec<String>>>;

const UNPOISONED: &str = "no code panics while holding the books";

/// Why a shelf refused something.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum ShelfError {
    /// The shelf is full.
    #[error("the shelf holds at most {limit} books")]
    #[bimetal(details)]
    Full {
        /// How many books the shelf holds.
        limit: u32,
        /// The book that did not fit.
        title: String,
    },
    /// A title is empty.
    #[error("a book needs a title")]
    Untitled,
    /// A visitor failed.
    #[error("the visitor failed: {reason}")]
    #[bimetal(details)]
    Visitor {
        /// What the visitor raised.
        reason: String,
    },
}

/// A shelf holds too few books. An error struct has one code.
#[derive(Debug, thiserror::Error, bimetal::Error)]
#[error("the shelf holds {held} books, and {wanted} are wanted")]
#[bimetal(details)]
pub struct Shortage {
    /// How many books the shelf holds.
    pub held: u32,
    /// How many books were wanted.
    pub wanted: u32,
}

impl From<CallbackError> for ShelfError {
    fn from(error: CallbackError) -> Self {
        Self::Visitor {
            reason: error.to_string(),
        }
    }
}

/// A named list of books.
pub struct Shelf {
    name: String,
    limit: u32,
    books: Books,
}

#[bimetal::export]
impl Shelf {
    /// Make an empty shelf.
    #[bimetal(constructor)]
    #[must_use]
    pub fn new(
        #[bimetal(default = "shelf")] name: String,
        #[bimetal(default = 3)] limit: u32,
    ) -> Self {
        Self {
            name,
            limit,
            books: Books::default(),
        }
    }

    /// The shelf's name.
    #[bimetal(getter)]
    #[must_use]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// Rename the shelf.
    #[bimetal(setter)]
    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// How many books the shelf holds.
    #[bimetal(len)]
    pub fn size(&self) -> usize {
        self.books.lock().expect(UNPOISONED).len()
    }

    /// Describe the shelf for logs.
    #[bimetal(repr)]
    pub fn describe(&self) -> String {
        format!("Shelf({:?}, {} books)", self.name, self.size())
    }

    /// Name the shelf for people.
    #[bimetal(display)]
    #[must_use]
    pub fn title(&self) -> String {
        format!("the {} shelf", self.name)
    }

    /// Whether the shelf holds a book.
    #[bimetal(contains)]
    pub fn has(&self, title: &str) -> bool {
        self.books
            .lock()
            .expect(UNPOISONED)
            .iter()
            .any(|book| book == title)
    }

    /// The book at a position, if there is one.
    #[bimetal(getitem)]
    pub fn at(&self, index: u32) -> Option<String> {
        self.books
            .lock()
            .expect(UNPOISONED)
            .get(index as usize)
            .cloned()
    }

    /// Replace the book at a position.
    #[bimetal(setitem)]
    pub fn replace(&mut self, index: u32, title: String) {
        if let Some(book) = self.books.lock().expect(UNPOISONED).get_mut(index as usize) {
            *book = title;
        }
    }

    /// Remove the book at a position.
    #[bimetal(delitem)]
    pub fn remove(&mut self, index: u32) {
        let mut books = self.books.lock().expect(UNPOISONED);
        if (index as usize) < books.len() {
            books.remove(index as usize);
        }
    }

    /// Whether the shelf holds any book.
    #[bimetal(bool)]
    pub fn occupied(&self) -> bool {
        self.size() > 0
    }

    /// The books whose titles contain a word.
    #[bimetal(call)]
    pub fn matching(&self, word: &str) -> Vec<String> {
        self.books
            .lock()
            .expect(UNPOISONED)
            .iter()
            .filter(|book| book.contains(word))
            .cloned()
            .collect()
    }

    /// A number that equal shelves share.
    #[bimetal(hash)]
    pub fn fingerprint(&self) -> u32 {
        self.books
            .lock()
            .expect(UNPOISONED)
            .iter()
            .map(|book| book.len() as u32)
            .sum()
    }

    /// Whether two shelves hold the same books.
    #[bimetal(eq)]
    pub fn same(&self, other: &Shelf) -> bool {
        *self.books.lock().expect(UNPOISONED) == *other.books.lock().expect(UNPOISONED)
    }

    /// Add a book, which a full shelf refuses.
    pub fn add(&mut self, title: &str) -> Result<(), ShelfError> {
        add(&self.books, self.limit, title)
    }

    /// Check that the shelf holds at least `wanted` books.
    pub fn require(&self, wanted: u32) -> Result<(), Shortage> {
        let held = self.size() as u32;
        if held < wanted {
            return Err(Shortage { held, wanted });
        }
        Ok(())
    }

    /// Open a session, whose books join the shelf when it commits.
    #[must_use]
    pub fn session(&self) -> Session {
        Session {
            limit: self.limit,
            books: Arc::clone(&self.books),
            pending: Vec::new(),
        }
    }

    /// Open a session that opens and ends through async calls.
    pub async fn async_session(&self) -> AsyncSession {
        AsyncSession {
            books: Arc::clone(&self.books),
            pending: Vec::new(),
            opened: false,
        }
    }

    /// Open a session, unless the shelf is full.
    #[must_use]
    pub fn try_session(&self) -> Option<Session> {
        (self.size() < self.limit as usize).then(|| self.session())
    }

    /// Divide the books among shelves of at most `size` books.
    #[must_use]
    pub fn split(&self, size: u32) -> Vec<Shelf> {
        self.books
            .lock()
            .expect(UNPOISONED)
            .chunks(size.max(1) as usize)
            .map(|books| Shelf {
                name: self.name.clone(),
                limit: self.limit,
                books: Arc::new(Mutex::new(books.to_vec())),
            })
            .collect()
    }

    /// Walk the titles, one call at a time.
    #[must_use]
    pub fn titles(&self) -> Titles {
        Titles {
            titles: self.books.lock().expect(UNPOISONED).clone().into_iter(),
        }
    }

    /// Walk the titles, one async call at a time.
    #[must_use]
    pub fn title_stream(&self) -> TitleStream {
        TitleStream {
            titles: self.books.lock().expect(UNPOISONED).clone().into_iter(),
        }
    }

    /// Copy the books of another shelf, which is only read.
    pub fn copy_from(&mut self, other: &Shelf) -> Result<(), ShelfError> {
        let titles = other.books.lock().expect(UNPOISONED).clone();
        for title in titles {
            add(&self.books, self.limit, &title)?;
        }
        Ok(())
    }

    /// Give the books to another shelf, which is held alone meanwhile.
    pub fn give_to(&self, other: &mut Shelf) {
        let books = std::mem::take(&mut *self.books.lock().expect(UNPOISONED));
        other.books.lock().expect(UNPOISONED).extend(books);
    }

    /// Take over another shelf, which is closed afterwards.
    pub fn absorb(&mut self, other: Shelf) {
        let books = std::mem::take(&mut *other.books.lock().expect(UNPOISONED));
        self.books.lock().expect(UNPOISONED).extend(books);
    }

    /// Count the books here and on another shelf, if one is passed.
    pub fn count_with(&self, other: Option<&Shelf>) -> u32 {
        (self.size() + other.map_or(0, Shelf::size)) as u32
    }

    /// Copy another shelf's books after a pause, while reading it.
    pub async fn copy_later(&mut self, other: &Shelf) -> Result<(), ShelfError> {
        pause(2).await;
        let titles = other.books.lock().expect(UNPOISONED).clone();
        for title in titles {
            add(&self.books, self.limit, &title)?;
        }
        Ok(())
    }

    /// Call `visitor` with each title and its position until it returns
    /// false, and return how many it saw.
    pub fn visit(&self, visitor: Callback<(String, u32), bool>) -> Result<u32, ShelfError> {
        let titles = self.books.lock().expect(UNPOISONED).clone();
        let mut seen = 0;
        for (index, title) in titles.into_iter().enumerate() {
            seen += 1;
            if !visitor.call((title, index as u32))? {
                break;
            }
        }
        Ok(seen)
    }

    /// Replace each title with what `transform` returns, holding the shelf
    /// alone meanwhile.
    pub fn rewrite(&mut self, transform: Callback<(String,), String>) -> Result<(), ShelfError> {
        let titles = self.books.lock().expect(UNPOISONED).clone();
        let mut rewritten = Vec::with_capacity(titles.len());
        for title in titles {
            rewritten.push(transform.call((title,))?);
        }
        *self.books.lock().expect(UNPOISONED) = rewritten;
        Ok(())
    }

    /// Call `label` from an async call, which may run on another thread.
    pub async fn label_later(
        &self,
        label: Callback<(String,), String>,
    ) -> Result<String, ShelfError> {
        pause(1).await;
        Ok(label.call((self.name.clone(),))?)
    }

    /// Call `label` from an async call that does not wait. A call that takes
    /// a host function starts on a worker on the native hosts all the same.
    pub async fn label_at_once(
        &self,
        label: Callback<(String,), String>,
    ) -> Result<String, ShelfError> {
        Ok(label.call((self.name.clone(),))?)
    }

    /// Keep `label` in a labeler, which calls it after this call returned.
    #[must_use]
    pub fn labeler(&self, label: Callback<(String,), String>) -> Labeler {
        Labeler { label }
    }

    /// The first title as bytes, if there is one.
    pub fn first_bytes(&self) -> Option<Vec<u8>> {
        self.at(0).map(String::into_bytes)
    }

    /// End the shelf, and return its books.
    pub fn seal(self) -> Vec<String> {
        std::mem::take(&mut *self.books.lock().expect(UNPOISONED))
    }

    /// End the shelf after a pause, and return how many books it held.
    pub async fn seal_later(self) -> usize {
        pause(1).await;
        self.size()
    }
}

/// Add a book, which a full shelf refuses.
fn add(books: &Books, limit: u32, title: &str) -> Result<(), ShelfError> {
    if title.is_empty() {
        return Err(ShelfError::Untitled);
    }
    let mut books = books.lock().expect(UNPOISONED);
    if books.len() >= limit as usize {
        return Err(ShelfError::Full {
            limit,
            title: title.to_owned(),
        });
    }
    books.push(title.to_owned());
    Ok(())
}

/// Books that join a shelf when the session commits, and that a `with` block
/// commits unless it raises.
pub struct Session {
    limit: u32,
    books: Books,
    pending: Vec<String>,
}

#[bimetal::export]
impl Session {
    /// Add a book to the session.
    pub fn add(&mut self, title: String) {
        self.pending.push(title);
    }

    /// Move the session's books to the shelf.
    pub fn commit(&mut self) -> Result<(), ShelfError> {
        for title in self.pending.drain(..) {
            add(&self.books, self.limit, &title)?;
        }
        Ok(())
    }

    /// End a `with` block: commit if it finished, and forget the books if it
    /// failed.
    #[bimetal(exit)]
    pub fn finish(&mut self, failed: bool) -> Result<(), ShelfError> {
        if failed {
            self.pending.clear();
            return Ok(());
        }
        self.commit()
    }
}

/// Books that an `async with` block commits, opening and ending through async
/// calls.
pub struct AsyncSession {
    books: Books,
    pending: Vec<String>,
    opened: bool,
}

#[bimetal::export]
impl AsyncSession {
    /// Start an `async with` block, and return how many books the shelf holds.
    #[bimetal(enter)]
    pub async fn open(&mut self) -> u32 {
        pause(1).await;
        self.opened = true;
        self.books.lock().expect(UNPOISONED).len() as u32
    }

    /// Whether the block started.
    #[bimetal(getter)]
    pub fn opened(&self) -> bool {
        self.opened
    }

    /// Add a book to the session.
    pub fn add(&mut self, title: String) {
        self.pending.push(title);
    }

    /// Move the session's books to the shelf.
    pub async fn commit(&mut self) {
        pause(1).await;
        let pending = std::mem::take(&mut self.pending);
        self.books.lock().expect(UNPOISONED).extend(pending);
    }

    /// End an `async with` block: commit if it finished, and forget the books
    /// if it failed.
    #[bimetal(exit)]
    pub async fn finish(&mut self, failed: bool) {
        if failed {
            self.pending.clear();
        } else {
            self.commit().await;
        }
    }
}

/// The titles of a shelf, one call at a time.
pub struct Titles {
    titles: vec::IntoIter<String>,
}

#[bimetal::export]
impl Titles {
    /// The next title, or nothing at the end.
    #[bimetal(next)]
    pub fn next_title(&mut self) -> Option<String> {
        self.titles.next()
    }
}

/// The titles of a shelf, one async call at a time.
pub struct TitleStream {
    titles: vec::IntoIter<String>,
}

#[bimetal::export]
impl TitleStream {
    /// The next title, or nothing at the end.
    #[bimetal(next)]
    pub async fn next_title(&mut self) -> Result<Option<String>, ShelfError> {
        pause(1).await;
        Ok(self.titles.next())
    }
}

/// Names shelves with a host function that it keeps.
pub struct Labeler {
    label: Callback<(String,), String>,
}

#[bimetal::export]
impl Labeler {
    /// Call the kept function.
    pub fn label(&self, name: String) -> Result<String, ShelfError> {
        Ok(self.label.call((name,))?)
    }
}

/// Count the books of two shelves, the second held alone.
#[bimetal::export]
#[must_use]
pub fn count_both(first: &Shelf, second: &mut Shelf) -> usize {
    first.size() + second.size()
}

/// The largest position that a pointer-sized argument reaches, doubled.
#[bimetal::export]
#[must_use]
pub fn double_size(size: usize) -> usize {
    size * 2
}

/// A number with a negative default.
#[bimetal::export]
#[must_use]
pub fn offset(
    value: i32,
    #[bimetal(default = -1)] by: i32,
    #[bimetal(default = 0.5)] scale: f64,
) -> f64 {
    f64::from(value + by) * scale
}
